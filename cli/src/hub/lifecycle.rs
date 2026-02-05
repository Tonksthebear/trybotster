//! Agent lifecycle management.
//!
//! This module provides functions for spawning and closing agents within the Hub.
//! It handles the core agent lifecycle operations:
//!
//! - Creating new agents from spawn configurations
//! - Setting up agent environment variables
//! - Registering agents with the Hub state
//! - Closing agents and optionally cleaning up worktrees
//!
//! # Architecture
//!
//! The lifecycle functions operate on [`HubState`] and return results that
//! the Hub can use to coordinate with other components (TUI, Relay, Tunnel Manager).
//!
//! ```text
//! SpawnConfig ──► spawn_agent() ──► Agent + Registration
//!                                        │
//!                    HubState ◄──────────┘
//! ```

// Rust guideline compliant 2025-01

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::agent::Agent;
use crate::agents::AgentSpawnConfig;
use crate::keyring::Credentials;
use crate::process::kill_orphaned_processes;

use super::HubState;

/// Result of spawning an agent, containing information for coordination.
#[derive(Debug)]
pub struct SpawnResult {
    /// The agent ID for the spawned agent.
    pub agent_id: String,
    /// The allocated HTTP forwarding port, if any.
    pub port: Option<u16>,
    /// Whether a server PTY was spawned.
    pub has_server_pty: bool,
}

/// Spawn a new agent with the given configuration.
///
/// This function:
/// 1. Creates a new Agent instance
/// 2. Sets up environment variables
/// 3. Writes the prompt file
/// 4. Copies the init script
/// 5. Spawns the CLI PTY
/// 6. Optionally spawns a server PTY (with allocated port stored on PtySession)
/// 7. Registers the agent with HubState
///
/// # Arguments
///
/// * `state` - Mutable reference to the Hub state
/// * `config` - Agent spawn configuration (includes dims for PTY sizing)
/// * `port` - Pre-allocated port for HTTP forwarding (from Hub.allocate_unique_port())
///
/// # Returns
///
/// A `SpawnResult` containing the session key and tunnel port information.
///
/// # Errors
///
/// Returns an error if:
/// - Writing the prompt file fails
/// - Copying the init script fails
/// - Spawning the PTY fails
pub fn spawn_agent(
    state: &mut HubState,
    config: &AgentSpawnConfig,
    port: Option<u16>,
) -> Result<SpawnResult> {
    let id = uuid::Uuid::new_v4();
    let mut agent = Agent::new_with_dims(
        id,
        config.repo_name.clone(),
        config.issue_number,
        config.branch_name.clone(),
        config.worktree_path.clone(),
        config.dims,
    );

    // Set invocation URL for notifications
    agent.last_invocation_url = config.invocation_url.clone().or_else(|| {
        config
            .issue_number
            .map(|num| format!("https://github.com/{}/issues/{num}", config.repo_name))
    });
    if let Some(ref url) = agent.last_invocation_url {
        log::info!("Agent invocation URL: {url}");
    }

    // Write prompt to .botster_prompt file
    let prompt_file_path = config.worktree_path.join(".botster_prompt");
    std::fs::write(&prompt_file_path, &config.prompt)
        .context("Failed to write .botster_prompt file")?;

    // Copy fresh .botster_init from main repo to worktree
    let source_init = config.repo_path.join(".botster_init");
    let dest_init = config.worktree_path.join(".botster_init");
    if source_init.exists() {
        std::fs::copy(&source_init, &dest_init)
            .context("Failed to copy .botster_init to worktree")?;
    }

    // Build environment variables
    let env_vars = build_spawn_environment(config);

    // Log the HTTP forwarding port (already allocated by Hub)
    if let Some(p) = port {
        log::info!("Using HTTP forwarding port {p} for agent");
    }

    // Kill any existing orphaned processes for this worktree
    kill_orphaned_processes(&config.worktree_path);

    // Spawn the agent with init commands
    let mut spawn_env = env_vars;
    if let Some(p) = port {
        spawn_env.insert("BOTSTER_TUNNEL_PORT".to_string(), p.to_string());
    }

    let init_commands = vec!["source .botster_init".to_string()];
    agent.spawn("bash", "", init_commands, &spawn_env)?;

    // Spawn server PTY if port is allocated and .botster_server exists
    // The port is stored on the server PtySession via set_port()
    let has_server_pty = if let Some(p) = port {
        spawn_server_pty_if_exists(&mut agent, &config.worktree_path, p)
    } else {
        false
    };

    // Register the agent
    let agent_id = agent.agent_id();
    let label = format_agent_label(config.issue_number, &config.branch_name);

    state.add_agent(agent_id.clone(), agent);
    log::info!("Spawned agent for {label}");

    Ok(SpawnResult {
        agent_id,
        port,
        has_server_pty,
    })
}

/// Close an agent and optionally delete its worktree.
///
/// # Arguments
///
/// * `state` - Mutable reference to the Hub state
/// * `agent_id` - The agent ID of the agent to close
/// * `delete_worktree` - Whether to delete the agent's worktree
///
/// # Returns
///
/// `Ok(true)` if the agent was found and closed, `Ok(false)` if not found.
///
/// # Errors
///
/// Returns an error if worktree deletion fails (but agent is still removed).
pub fn close_agent(state: &mut HubState, agent_id: &str, delete_worktree: bool) -> Result<bool> {
    let Some(agent) = state.remove_agent(agent_id) else {
        log::info!("No agent found with agent ID: {agent_id}");
        return Ok(false);
    };

    let label = format_agent_label(agent.issue_number, &agent.branch_name);

    if delete_worktree {
        if let Err(e) = state
            .git_manager
            .delete_worktree_by_path(&agent.worktree_path, &agent.branch_name)
        {
            log::error!("Failed to delete worktree for {label}: {e}");
            // Still return Ok since agent was removed
        } else {
            log::info!("Closed agent and deleted worktree for {label}");
        }
    } else {
        log::info!("Closed agent for {label} (worktree preserved)");
    }

    Ok(true)
}

/// Build environment variables for agent spawn.
///
/// Creates a HashMap of environment variables needed by the agent process.
/// Also overrides certain inherited variables that could cause issues
/// (e.g., RUST_LOG=debug causes Claude Code to dump verbose output).
fn build_spawn_environment(config: &AgentSpawnConfig) -> HashMap<String, String> {
    let mut env_vars = HashMap::new();

    env_vars.insert("BOTSTER_REPO".to_string(), config.repo_name.clone());
    env_vars.insert(
        "BOTSTER_ISSUE_NUMBER".to_string(),
        config
            .issue_number
            .map_or_else(|| "0".to_string(), |n| n.to_string()),
    );
    env_vars.insert(
        "BOTSTER_BRANCH_NAME".to_string(),
        config.branch_name.clone(),
    );
    env_vars.insert(
        "BOTSTER_WORKTREE_PATH".to_string(),
        config.worktree_path.display().to_string(),
    );
    env_vars.insert(
        "BOTSTER_TASK_DESCRIPTION".to_string(),
        config.prompt.clone(),
    );

    if let Some(msg_id) = config.message_id {
        env_vars.insert("BOTSTER_MESSAGE_ID".to_string(), msg_id.to_string());
    }

    // Add the hub binary path for subprocesses
    let bin_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(std::string::ToString::to_string))
        .unwrap_or_else(|| "botster-hub".to_string());
    env_vars.insert("BOTSTER_HUB_BIN".to_string(), bin_path);

    // Add MCP token for agent authentication (scoped to MCP operations only)
    if let Ok(creds) = Credentials::load() {
        if let Some(mcp_token) = creds.mcp_token() {
            env_vars.insert("BOTSTER_MCP_TOKEN".to_string(), mcp_token.to_string());
        }
    }

    env_vars
}

/// Spawn a server PTY if .botster_server exists.
///
/// After spawning, stores the allocated port on the server PtySession via `set_port()`.
/// This allows browser clients to query the port when needed for preview proxying.
///
/// Returns true if a server PTY was successfully spawned.
fn spawn_server_pty_if_exists(agent: &mut Agent, worktree_path: &Path, port: u16) -> bool {
    let server_script = worktree_path.join(".botster_server");
    if !server_script.exists() {
        return false;
    }

    log::info!("Spawning server PTY on port {port} using .botster_server");

    let mut server_env = HashMap::new();
    server_env.insert("BOTSTER_TUNNEL_PORT".to_string(), port.to_string());
    server_env.insert(
        "BOTSTER_WORKTREE_PATH".to_string(),
        worktree_path.display().to_string(),
    );

    match agent.spawn_server_pty(".botster_server", &server_env) {
        Ok(()) => {
            // Store the port on the server PtySession for browser clients to query
            if let Some(ref mut server_pty) = agent.server_pty {
                server_pty.set_port(port);
                log::debug!("Set port {port} on server PTY");
            }
            true
        }
        Err(e) => {
            log::warn!("Failed to spawn server PTY: {e}");
            false
        }
    }
}

/// Format a human-readable label for an agent.
fn format_agent_label(issue_number: Option<u32>, branch_name: &str) -> String {
    if let Some(num) = issue_number {
        format!("issue #{num}")
    } else {
        format!("branch {branch_name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_build_spawn_environment() {
        let config = AgentSpawnConfig {
            issue_number: Some(42),
            branch_name: "issue-42".to_string(),
            worktree_path: PathBuf::from("/tmp/worktree"),
            repo_path: PathBuf::from("/tmp/repo"),
            repo_name: "owner/repo".to_string(),
            prompt: "Fix the bug".to_string(),
            message_id: Some(123),
            invocation_url: None,
            dims: (24, 80),
        };

        let env = build_spawn_environment(&config);

        assert_eq!(env.get("BOTSTER_REPO"), Some(&"owner/repo".to_string()));
        assert_eq!(env.get("BOTSTER_ISSUE_NUMBER"), Some(&"42".to_string()));
        assert_eq!(
            env.get("BOTSTER_BRANCH_NAME"),
            Some(&"issue-42".to_string())
        );
        assert_eq!(env.get("BOTSTER_MESSAGE_ID"), Some(&"123".to_string()));
        assert!(env.contains_key("BOTSTER_HUB_BIN"));
    }

    #[test]
    fn test_build_spawn_environment_no_issue() {
        let config = AgentSpawnConfig {
            issue_number: None,
            branch_name: "feature-branch".to_string(),
            worktree_path: PathBuf::from("/tmp/worktree"),
            repo_path: PathBuf::from("/tmp/repo"),
            repo_name: "owner/repo".to_string(),
            prompt: "Work on feature".to_string(),
            message_id: None,
            invocation_url: None,
            dims: (24, 80),
        };

        let env = build_spawn_environment(&config);

        assert_eq!(env.get("BOTSTER_ISSUE_NUMBER"), Some(&"0".to_string()));
        assert!(!env.contains_key("BOTSTER_MESSAGE_ID"));
    }

    #[test]
    fn test_format_agent_label_with_issue() {
        let label = format_agent_label(Some(42), "issue-42");
        assert_eq!(label, "issue #42");
    }

    #[test]
    fn test_format_agent_label_without_issue() {
        let label = format_agent_label(None, "feature-branch");
        assert_eq!(label, "branch feature-branch");
    }

    #[test]
    fn test_close_agent_not_found() {
        let mut state = HubState::new(PathBuf::from("/tmp/worktrees"));
        let result = close_agent(&mut state, "nonexistent-key", false).unwrap();
        assert!(!result);
    }

    #[test]
    fn test_build_spawn_environment_includes_mcp_token() {
        use crate::keyring::Credentials;

        // Set up credentials with MCP token (test mode uses file storage)
        let mut creds = Credentials::load().unwrap_or_default();
        creds.set_mcp_token("btmcp_test_agent_token".to_string());
        creds.save().unwrap();

        let config = AgentSpawnConfig {
            issue_number: Some(1),
            branch_name: "test".to_string(),
            worktree_path: PathBuf::from("/tmp/worktree"),
            repo_path: PathBuf::from("/tmp/repo"),
            repo_name: "test/repo".to_string(),
            prompt: "Test".to_string(),
            message_id: None,
            invocation_url: None,
            dims: (24, 80),
        };

        let env = build_spawn_environment(&config);

        // MCP token should be in the environment
        assert_eq!(
            env.get("BOTSTER_MCP_TOKEN"),
            Some(&"btmcp_test_agent_token".to_string())
        );

        // Clean up
        creds.clear_mcp_token();
        creds.save().unwrap();
    }
}
