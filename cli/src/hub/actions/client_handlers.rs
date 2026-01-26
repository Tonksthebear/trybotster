//! Client-scoped action handlers.
//!
//! Handlers for actions that operate on a specific client's view,
//! including selection, input routing, and agent management.
//!
//! # Architecture
//!
//! Hub communicates with clients via `ClientCmd` channels, not by calling
//! trait methods directly. Each client runs its own async task that
//! processes commands from Hub and requests from its input source.
//!
//! This module handles high-level client actions:
//!
//! - Agent selection: `handle_select_agent_for_client()`
//! - Agent creation/deletion: `handle_create_agent_for_client()`, `handle_delete_agent_for_client()`
//! - Client lifecycle: `handle_client_connected()`, `handle_client_disconnected()`

// Rust guideline compliant 2026-01

use std::path::PathBuf;
use std::sync::Arc;

use crate::client::{BrowserClient, ClientCmd, ClientId, ClientTaskHandle, CreateAgentRequest, DeleteAgentRequest};
use crate::client::browser::BrowserClientConfig;
use crate::hub::{lifecycle, Hub};

/// Handle selecting an agent for a specific client.
///
/// When a client selects an agent:
/// 1. Validates agent exists
/// 2. Ensures agent's channels are connected (lazy connection)
///
/// TUI selection state is owned by TuiRunner, not Hub.
/// PTY connection is handled separately by each client type via their
/// async task loops (TuiRequest::SelectAgent, BrowserEvent::SelectAgent).
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

    log::debug!(
        "Client {} selected agent {}",
        client_id,
        &agent_key[..8.min(agent_key.len())]
    );
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

    // Resolve dims: use request dims if provided, otherwise default (24, 80)
    let dims = request.dims.unwrap_or((24, 80));

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
            dims,
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
                        dims,
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
            dims,
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
    dims: (u16, u16),
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
        dims,
    };

    // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
    let _runtime_guard = hub.tokio_runtime.enter();

    // Spawn agent - release lock before continuing
    let spawn_result = {
        let mut state = hub.state.write().unwrap();
        lifecycle::spawn_agent(&mut state, &config)
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
/// 1. All clients are notified to disconnect from that agent's PTYs via ClientCmd
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

    // Broadcast disconnect commands to all clients for this agent's PTYs
    if let Some(idx) = agent_index {
        // Disconnect from CLI PTY (index 0) and Server PTY (index 1)
        for (_, handle) in hub.clients.iter() {
            let _ = handle.cmd_tx.try_send(ClientCmd::DisconnectFromPty { agent_index: idx, pty_index: 0 });
            let _ = handle.cmd_tx.try_send(ClientCmd::DisconnectFromPty { agent_index: idx, pty_index: 1 });
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
///
/// For browser clients, creates a BrowserClient, spawns it as an async task,
/// and registers the task handle in the client registry.
pub fn handle_client_connected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client connected: {}", client_id);

    // For browser clients, create and register BrowserClient as async task
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
        let browser_client = BrowserClient::new(hub_handle, identity.clone(), config);

        // Create Hub -> Client command channel
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(64);

        // Spawn BrowserClient as async task
        let join_handle = hub.tokio_runtime.spawn(browser_client.run_task(cmd_rx));

        // Register the task handle
        hub.clients.register(client_id.clone(), ClientTaskHandle {
            cmd_tx,
            join_handle,
        });

        log::info!("Registered BrowserClient task for {}", identity);
    }
}

/// Handle client disconnected event.
///
/// When a client disconnects:
/// 1. Send Shutdown command to the client's async task
/// 2. Abort the task and unregister from the registry
pub fn handle_client_disconnected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client disconnecting: {}", client_id);

    // Unregister the client task handle - dropping it closes the command channel
    if let Some(handle) = hub.clients.unregister(&client_id) {
        // Send shutdown command before aborting
        let _ = handle.cmd_tx.try_send(ClientCmd::Shutdown);
        handle.join_handle.abort();
    }

    log::info!("Client disconnected: {}", client_id);
}
