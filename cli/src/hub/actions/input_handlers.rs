//! Agent spawn helpers.
//!
//! Helper to spawn an agent and connect its channels (used by TUI menu flow).

use std::sync::Arc;

use crate::client::ClientId;
use crate::hub::{lifecycle, Hub};

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
    // Enter tokio runtime context for spawn_command_processor() which uses tokio::spawn()
    let _runtime_guard = hub.tokio_runtime.enter();

    // Dims are carried in config from the requesting client
    let result = lifecycle::spawn_agent(&mut hub.state.write().unwrap(), config)?;

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
