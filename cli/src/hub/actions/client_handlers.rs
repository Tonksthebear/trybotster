//! Client-scoped action handlers.
//!
//! Handlers for actions that operate on a specific client's view,
//! including selection, input routing, and agent management.
//!
//! # Architecture
//!
//! Hub communicates with TUI via HubEvent broadcasts. Browser communication
//! happens directly via WebRTC in `server_comms.rs`, bypassing the Client
//! trait entirely.
//!
//! This module handles high-level client actions:
//!
//! - Agent selection: `handle_select_agent_for_client()`
//! - Agent creation/deletion: `handle_create_agent_for_client()`, `handle_delete_agent_for_client()`
//! - Client lifecycle: `handle_client_connected()`, `handle_client_disconnected()` (legacy no-ops)

// Rust guideline compliant 2026-01

use std::path::PathBuf;

use crate::client::{ClientId, CreateAgentRequest, DeleteAgentRequest};
use crate::hub::{lifecycle, Hub};

/// Handle selecting an agent for a specific client.
///
/// When a client selects an agent:
/// 1. Validates agent exists
/// 2. Ensures agent's channels are connected (lazy connection)
///
/// TUI selection state is owned by TuiRunner, not Hub.
/// PTY connection is handled separately:
/// - TUI: TuiRequest::SelectAgent triggers PTY connection
/// - Browser: terminal_connected event triggers PtyConnectionRequested broadcast
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
    // Broadcast creation progress to all subscribers (WebRTC, TUI)
    hub.broadcast(crate::hub::HubEvent::AgentCreationProgress {
        identifier: request.issue_or_branch.clone(),
        stage: crate::relay::AgentCreationStage::CreatingWorktree,
    });

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
                port: None,
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

    // Allocate a unique port for HTTP forwarding (before spawning)
    let port = hub.allocate_unique_port();

    // Spawn agent - release lock before continuing
    let spawn_result = {
        let mut state = hub.state.write().unwrap();
        lifecycle::spawn_agent(&mut state, &config, port)
    };

    match spawn_result {
        Ok(result) => {
            log::info!("Client {} created agent: {}", client_id, result.agent_id);

            // Sync handle cache for thread-safe agent access
            hub.sync_handle_cache();

            // Connect agent's channels (terminal + preview if port assigned)
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

            let agent_id = result.agent_id;

            // Agent list broadcast is handled via HubEvent::AgentCreated below
            // WebRTC and TUI react to this event
            hub.broadcast_agent_list();
            handle_select_agent_for_client(hub, client_id, agent_id.clone());

            // Refresh worktree cache - this agent's worktree is now in use
            if let Err(e) = hub.load_available_worktrees() {
                log::warn!("Failed to refresh worktree cache after agent creation: {}", e);
            }

            // Broadcast AgentCreated event to all subscribers (including TUI)
            if let Some(info) = hub.state.read().unwrap().get_agent_info(&agent_id) {
                hub.broadcast(crate::hub::HubEvent::agent_created(agent_id.clone(), info.clone()));

                // Fire Lua event for agent_created
                if let Err(e) = hub.lua.fire_agent_created(&agent_id, &info) {
                    log::warn!("Lua agent_created event error: {}", e);
                }
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
/// 1. HubEvent::AgentDeleted is broadcast -- each client's handle_hub_event()
///    disconnects from the deleted agent's PTYs.
/// 2. The agent and optionally its worktree are deleted.
pub fn handle_delete_agent_for_client(
    hub: &mut Hub,
    client_id: ClientId,
    request: DeleteAgentRequest,
) {
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

            // Broadcast AgentDeleted event to all subscribers (TUI, WebRTC)
            hub.broadcast(crate::hub::HubEvent::agent_deleted(&request.agent_id));

            // Fire Lua event for agent_deleted
            if let Err(e) = hub.lua.fire_agent_deleted(&request.agent_id) {
                log::warn!("Lua agent_deleted event error: {}", e);
            }

            // Refresh worktree cache - this agent's worktree is now available
            if let Err(e) = hub.load_available_worktrees() {
                log::warn!("Failed to refresh worktree cache after agent deletion: {}", e);
            }
        }
        Err(e) => {
            hub.send_error_to(&client_id, format!("Failed to delete agent: {}", e));
        }
    }
}

/// Handle client connected event.
///
/// This is a legacy handler for browser_connected events that are no longer sent.
/// Browser communication now happens directly via WebRTC in `server_comms.rs`,
/// bypassing the Client trait and ClientRegistry entirely.
///
/// This handler is retained for backward compatibility but is effectively a no-op.
pub fn handle_client_connected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client connected: {} (no-op - WebRTC handles browser connections directly)", client_id);
    // Suppress unused variable warning
    let _ = hub;
}

/// Handle client disconnected event.
///
/// This is a legacy handler for browser_disconnected events that are no longer sent.
/// Browser communication now happens directly via WebRTC in `server_comms.rs`.
///
/// This handler is retained for backward compatibility but is effectively a no-op.
pub fn handle_client_disconnected(hub: &mut Hub, client_id: ClientId) {
    log::info!("Client disconnected: {} (no-op - WebRTC handles browser connections directly)", client_id);
    // Suppress unused variable warning
    let _ = hub;
}
