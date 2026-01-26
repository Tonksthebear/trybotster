//! Agent lifecycle handlers - spawn and close operations.

use std::path::PathBuf;
use std::sync::Arc;

use crate::hub::{lifecycle, Hub};

/// Handle spawning a new agent.
///
/// Creates a worktree and spawns an agent for the given branch/issue.
/// This is typically triggered by server messages (GitHub webhooks).
pub fn handle_spawn_agent(
    hub: &mut Hub,
    issue_number: Option<u32>,
    branch_name: String,
    _worktree_path: PathBuf, // Ignored - we create the worktree ourselves
    repo_path: PathBuf,
    repo_name: String,
    prompt: String,
    message_id: Option<i64>,
    invocation_url: Option<String>,
) {
    log::debug!(
        "SpawnAgent: branch={}, issue={:?}",
        branch_name,
        issue_number
    );

    // Create the worktree first (the path in the action is just computed, not created)
    let worktree_path = match hub
        .state
        .write()
        .unwrap()
        .git_manager
        .create_worktree_with_branch(&branch_name)
    {
        Ok(path) => {
            log::info!("Worktree created at {:?}", path);
            path
        }
        Err(e) => {
            log::error!("Failed to create worktree for {}: {}", branch_name, e);
            return;
        }
    };

    let config = crate::agents::AgentSpawnConfig {
        issue_number,
        branch_name,
        worktree_path,
        repo_path,
        repo_name,
        prompt,
        message_id,
        invocation_url,
        // Server-initiated agents (webhooks) have no client providing dims,
        // so use a reasonable default.
        dims: (24, 80),
    };

    // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
    let _runtime_guard = hub.tokio_runtime.enter();

    match lifecycle::spawn_agent(&mut hub.state.write().unwrap(), &config) {
        Ok(result) => {
            log::info!("Spawned agent: {}", result.agent_id);

            // Sync handle cache for thread-safe agent access
            hub.sync_handle_cache();

            if let Some(port) = result.tunnel_port {
                let tm = Arc::clone(&hub.tunnel_manager);
                let key = result.agent_id.clone();
                hub.tokio_runtime.spawn(async move {
                    tm.register_agent(key, port).await;
                });
            }
        }
        Err(e) => log::error!("Failed to spawn agent: {}", e),
    }
}

/// Handle closing an agent.
///
/// Closes the agent identified by session_key, optionally deleting its worktree.
pub fn handle_close_agent(hub: &mut Hub, session_key: &str, delete_worktree: bool) {
    log::debug!("CloseAgent: session_key={}", session_key);
    if let Err(e) = lifecycle::close_agent(
        &mut hub.state.write().unwrap(),
        session_key,
        delete_worktree,
    ) {
        log::error!("Failed to close agent {}: {}", session_key, e);
    } else {
        // Sync handle cache for thread-safe agent access
        hub.sync_handle_cache();
    }
}

