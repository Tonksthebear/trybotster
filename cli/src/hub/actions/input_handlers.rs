//! Agent spawn helpers.
//!
//! Helper to spawn an agent and connect its channels (used by TUI menu flow).

use crate::client::ClientId;
use crate::hub::{lifecycle, Hub};

/// Helper to spawn an agent and connect its channels.
///
/// This is used by TUI's "New Agent" menu flow. After spawning:
/// - Connects agent's channels (terminal + preview if port assigned)
/// - Auto-selects the new agent for TUI (consistent with browser behavior)
pub fn spawn_agent_with_tunnel(
    hub: &mut Hub,
    config: &crate::agents::AgentSpawnConfig,
) -> anyhow::Result<()> {
    // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
    let _runtime_guard = hub.tokio_runtime.enter();

    // Allocate a unique port for HTTP forwarding (before spawning)
    let port = hub.allocate_unique_port();

    // Dims are carried in config from the requesting client
    let result = lifecycle::spawn_agent(&mut hub.state.write().unwrap(), config, port)?;

    // Clone agent_id before connecting channels
    let agent_id = result.agent_id.clone();

    // Connect agent's channels (terminal always, preview if port assigned)
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
