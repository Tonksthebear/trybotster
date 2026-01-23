//! Text input handlers.
//!
//! Handlers for text input processing and agent creation from input.

use std::sync::Arc;

use crate::app::AppMode;
use crate::client::ClientId;
use crate::hub::{lifecycle, Hub};

use super::client_handlers::handle_create_agent_for_client;

/// Handle input submission based on current mode.
pub fn handle_input_submit(hub: &mut Hub) {
    match hub.mode {
        AppMode::NewAgentCreateWorktree => {
            if !hub.input_buffer.is_empty() {
                if let Err(e) = create_and_spawn_agent(hub) {
                    log::error!("Failed to create worktree and spawn agent: {}", e);
                }
            }
        }
        AppMode::NewAgentPrompt => {
            if let Err(e) = spawn_agent_from_worktree(hub) {
                log::error!("Failed to spawn agent: {}", e);
            }
        }
        _ => {}
    }
    hub.mode = AppMode::Normal;
    hub.input_buffer.clear();
}

/// Spawn an agent from a selected existing worktree.
fn spawn_agent_from_worktree(hub: &mut Hub) -> anyhow::Result<()> {
    let worktree_index = hub.worktree_selected.saturating_sub(1);

    // Get worktree info in a separate scope to release the lock
    let worktree_info = hub
        .state
        .read()
        .unwrap()
        .available_worktrees
        .get(worktree_index)
        .cloned();

    if let Some((path, branch)) = worktree_info {
        let issue_number = branch
            .strip_prefix("botster-issue-")
            .and_then(|s| s.parse::<u32>().ok());

        let (repo_path, repo_name) = crate::git::WorktreeManager::detect_current_repo()?;
        let worktree_path = std::path::PathBuf::from(&path);

        let prompt = if hub.input_buffer.is_empty() {
            issue_number.map_or_else(
                || format!("Work on {branch}"),
                |n| format!("Work on issue #{n}"),
            )
        } else {
            hub.input_buffer.clone()
        };

        let config = crate::agents::AgentSpawnConfig {
            issue_number,
            branch_name: branch,
            worktree_path,
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None,
        };
        spawn_agent_with_tunnel(hub, &config)?;
    }

    Ok(())
}

/// Create a new worktree and spawn an agent on it.
///
/// Routes through the async `handle_create_agent_for_client` path to avoid
/// blocking the TUI during git operations.
fn create_and_spawn_agent(hub: &mut Hub) -> anyhow::Result<()> {
    let branch_name = hub.input_buffer.trim();

    if branch_name.is_empty() {
        anyhow::bail!("Branch name cannot be empty");
    }

    // Route through the async client path (same as browser)
    // This spawns git operations to background thread
    let request = crate::client::CreateAgentRequest {
        issue_or_branch: branch_name.to_string(),
        prompt: None,
        from_worktree: None,
    };

    handle_create_agent_for_client(hub, ClientId::Tui, request);
    Ok(())
}

/// Helper to spawn an agent and connect its channels.
///
/// This is used by TUI's "New Agent" menu flow. After spawning:
/// - Registers tunnel if port assigned
/// - Connects agent's channels (terminal + preview if tunnel exists)
/// - Auto-selects the new agent for TUI (consistent with browser behavior)
pub fn spawn_agent_with_tunnel(
    hub: &mut Hub,
    config: &crate::agents::AgentSpawnConfig,
) -> anyhow::Result<()> {
    // Use TUI's dims from terminal_dims (not browser.dims)
    let dims = hub.terminal_dims;

    let result = lifecycle::spawn_agent(&mut hub.state.write().unwrap(), config, dims)?;

    // Clone agent_id before moving into async
    let agent_id = result.agent_id.clone();

    // Register tunnel for HTTP forwarding if tunnel port allocated
    if let Some(port) = result.tunnel_port {
        let tm = Arc::clone(&hub.tunnel_manager);
        let key = result.agent_id.clone();
        hub.tokio_runtime.spawn(async move {
            tm.register_agent(key, port).await;
        });
    }

    // Connect agent's channels (terminal always, preview if tunnel_port set)
    // Agent owns its channels per spec Section 7
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

    // Auto-select the new agent for TUI (matches browser behavior in handle_create_agent_for_client)
    super::client_handlers::handle_select_agent_for_client(hub, ClientId::Tui, agent_id);

    Ok(())
}
