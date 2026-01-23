//! Client registry with optimized viewer tracking.
//!
//! The registry maintains all connected clients and provides O(1) lookup
//! for PTY output routing via a reverse index.
//!
//! # Selection State
//!
//! Selection state (which agent each client is viewing) is owned by the registry,
//! not by the Client trait. This keeps the Client trait focused on IO concerns
//! while the registry handles routing state.

use std::collections::{HashMap, HashSet};

use super::{Client, ClientId};

/// Registry of all connected clients with reverse index for PTY routing.
///
/// # Reverse Index
///
/// The `viewers` HashMap maps agent keys to sets of client IDs viewing that agent.
/// This enables O(1) lookup when routing PTY output, instead of iterating all clients.
///
/// # Selection Tracking
///
/// The `selections` HashMap tracks which agent each client is viewing.
/// This is the authoritative source for selection state - the Client trait
/// does not expose selection state.
///
/// # Example
///
/// ```text
/// viewers: {
///     "agent-abc": { ClientId::Tui, ClientId::Browser("xyz") },
///     "agent-def": { ClientId::Browser("123") },
/// }
/// selections: {
///     ClientId::Tui: "agent-abc",
///     ClientId::Browser("xyz"): "agent-abc",
///     ClientId::Browser("123"): "agent-def",
/// }
/// ```
///
/// When agent "agent-abc" produces output, we can immediately find all viewers.
pub struct ClientRegistry {
    /// All clients by ID.
    clients: HashMap<ClientId, Box<dyn Client>>,

    /// Reverse index: agent_id -> set of client IDs viewing that agent.
    /// Enables O(1) lookup for PTY output routing.
    viewers: HashMap<String, HashSet<ClientId>>,

    /// Forward index: client_id -> selected agent_id.
    /// Registry owns selection state since Client trait doesn't expose it.
    selections: HashMap<ClientId, String>,
}

impl ClientRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            viewers: HashMap::new(),
            selections: HashMap::new(),
        }
    }

    /// Register a new client.
    ///
    /// Client starts with no selection. Call `select_agent` to set selection.
    pub fn register(&mut self, client: Box<dyn Client>) {
        let id = client.id().clone();
        self.clients.insert(id, client);
    }

    /// Unregister a client, cleaning up viewer index and selection state.
    ///
    /// Returns the removed client if it existed.
    pub fn unregister(&mut self, id: &ClientId) -> Option<Box<dyn Client>> {
        if let Some(client) = self.clients.remove(id) {
            // Remove from viewer index using our tracked selection
            if let Some(agent_id) = self.selections.remove(id) {
                if let Some(viewers) = self.viewers.get_mut(&agent_id) {
                    viewers.remove(id);
                    if viewers.is_empty() {
                        self.viewers.remove(&agent_id);
                    }
                }
            }
            Some(client)
        } else {
            None
        }
    }

    /// Get client by ID (immutable).
    pub fn get(&self, id: &ClientId) -> Option<&dyn Client> {
        self.clients.get(id).map(|c| c.as_ref())
    }

    /// Get client by ID (mutable).
    pub fn get_mut(&mut self, id: &ClientId) -> Option<&mut Box<dyn Client>> {
        self.clients.get_mut(id)
    }

    /// Set the selected agent for a client.
    ///
    /// This updates both the forward index (client -> agent) and
    /// the reverse index (agent -> clients).
    ///
    /// # Arguments
    ///
    /// * `client_id` - The client changing selection
    /// * `agent_id` - The agent to select (None to clear selection)
    pub fn select_agent(&mut self, client_id: &ClientId, agent_id: Option<&str>) {
        // Remove from old agent's viewers if there was a selection
        if let Some(old_id) = self.selections.remove(client_id) {
            if let Some(viewers) = self.viewers.get_mut(&old_id) {
                viewers.remove(client_id);
                if viewers.is_empty() {
                    self.viewers.remove(&old_id);
                }
            }
        }

        // Add to new agent's viewers
        if let Some(new_id) = agent_id {
            self.selections
                .insert(client_id.clone(), new_id.to_string());
            self.viewers
                .entry(new_id.to_string())
                .or_default()
                .insert(client_id.clone());
        }
    }

    /// Get the selected agent for a client.
    pub fn selected_agent(&self, client_id: &ClientId) -> Option<&str> {
        self.selections.get(client_id).map(|s| s.as_str())
    }

    /// Clear selection for a client (convenience method).
    pub fn clear_selection(&mut self, client_id: &ClientId) {
        self.select_agent(client_id, None);
    }

    /// Get all client IDs viewing a specific agent (O(1) lookup).
    ///
    /// Returns an empty iterator if no clients are viewing the agent.
    pub fn viewers_of(&self, agent_id: &str) -> impl Iterator<Item = &ClientId> {
        self.viewers
            .get(agent_id)
            .into_iter()
            .flat_map(|set| set.iter())
    }

    /// Iterate all clients (immutable).
    pub fn iter(&self) -> impl Iterator<Item = (&ClientId, &Box<dyn Client>)> {
        self.clients.iter()
    }

    /// Iterate all clients (mutable).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ClientId, &mut Box<dyn Client>)> {
        self.clients.iter_mut()
    }

    /// Get all client IDs.
    pub fn client_ids(&self) -> impl Iterator<Item = &ClientId> {
        self.clients.keys()
    }

    /// Number of connected clients.
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Remove agent from all viewer indices (when agent is deleted).
    ///
    /// This clears the selection for all clients that were viewing this agent,
    /// updating both the forward and reverse indices.
    pub fn remove_agent_viewers(&mut self, agent_id: &str) {
        // Get all clients viewing this agent
        if let Some(viewer_ids) = self.viewers.remove(agent_id) {
            // Clear their selection in the forward index
            for client_id in viewer_ids {
                self.selections.remove(&client_id);
            }
        }
    }

    /// Get count of viewers for a specific agent.
    pub fn viewer_count(&self, agent_id: &str) -> usize {
        self.viewers.get(agent_id).map(|set| set.len()).unwrap_or(0)
    }
}

impl Default for ClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ClientRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRegistry")
            .field("client_count", &self.clients.len())
            .field("viewer_count", &self.viewers.len())
            .field("selection_count", &self.selections.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::types::CreateAgentRequest;
    use crate::client::{AgentHandle, AgentInfo};

    /// Test client implementing the event-driven Client trait.
    struct TestClient {
        id: ClientId,
        dims: (u16, u16),
        connected: bool,
        output_received: Vec<u8>,
    }

    impl TestClient {
        fn new(id: ClientId) -> Self {
            Self {
                id,
                dims: (80, 24),
                connected: true,
                output_received: Vec::new(),
            }
        }
    }

    impl Client for TestClient {
        fn id(&self) -> &ClientId {
            &self.id
        }

        fn dims(&self) -> (u16, u16) {
            self.dims
        }

        fn get_agents(&self) -> Vec<AgentInfo> {
            // Test client returns empty
            Vec::new()
        }

        fn get_agent(&self, _index: usize) -> Option<AgentHandle> {
            // Test client returns None
            None
        }

        fn request_create_agent(&self, _request: CreateAgentRequest) -> Result<(), String> {
            Ok(())
        }

        fn request_delete_agent(&self, _agent_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn on_output(&mut self, data: &[u8]) {
            self.output_received.extend_from_slice(data);
        }

        fn on_resized(&mut self, _rows: u16, _cols: u16) {}

        fn on_process_exit(&mut self, _exit_code: Option<i32>) {}

        fn on_agent_created(&mut self, _index: usize, _info: &AgentInfo) {}

        fn on_agent_deleted(&mut self, _index: usize) {}

        fn on_hub_shutdown(&mut self) {}

        fn is_connected(&self) -> bool {
            self.connected
        }
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        assert_eq!(registry.len(), 1);
        assert!(registry.get(&ClientId::Tui).is_some());
        assert!(registry
            .get(&ClientId::Browser("xyz".to_string()))
            .is_none());
    }

    #[test]
    fn test_unregister_clears_selection() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        // Select an agent via registry
        registry.select_agent(&ClientId::Tui, Some("agent-123"));
        assert_eq!(registry.viewer_count("agent-123"), 1);

        // Unregister - should clean up selection
        let removed = registry.unregister(&ClientId::Tui);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.viewer_count("agent-123"), 0);
    }

    #[test]
    fn test_select_agent_updates_indices() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        // Select an agent
        registry.select_agent(&ClientId::Tui, Some("agent-abc"));

        // Should be in viewer index
        let viewers: Vec<_> = registry.viewers_of("agent-abc").collect();
        assert_eq!(viewers.len(), 1);
        assert_eq!(viewers[0], &ClientId::Tui);

        // Should be in selection index
        assert_eq!(registry.selected_agent(&ClientId::Tui), Some("agent-abc"));
    }

    #[test]
    fn test_select_agent_changes_selection() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        // Initial selection
        registry.select_agent(&ClientId::Tui, Some("agent-old"));
        assert_eq!(registry.viewer_count("agent-old"), 1);

        // Change selection
        registry.select_agent(&ClientId::Tui, Some("agent-new"));

        // Verify indices updated
        assert_eq!(registry.viewer_count("agent-old"), 0);
        assert_eq!(registry.viewer_count("agent-new"), 1);
        assert_eq!(registry.selected_agent(&ClientId::Tui), Some("agent-new"));
    }

    #[test]
    fn test_clear_selection() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        registry.select_agent(&ClientId::Tui, Some("agent-123"));
        assert_eq!(registry.viewer_count("agent-123"), 1);

        // Clear selection
        registry.clear_selection(&ClientId::Tui);

        assert_eq!(registry.viewer_count("agent-123"), 0);
        assert_eq!(registry.selected_agent(&ClientId::Tui), None);
    }

    #[test]
    fn test_multiple_viewers() {
        let mut registry = ClientRegistry::new();

        let tui = TestClient::new(ClientId::Tui);
        let browser = TestClient::new(ClientId::browser("xyz"));

        registry.register(Box::new(tui));
        registry.register(Box::new(browser));

        // Both select the same agent via registry
        registry.select_agent(&ClientId::Tui, Some("agent-shared"));
        registry.select_agent(&ClientId::browser("xyz"), Some("agent-shared"));

        assert_eq!(registry.viewer_count("agent-shared"), 2);

        let viewers: HashSet<_> = registry.viewers_of("agent-shared").cloned().collect();
        assert!(viewers.contains(&ClientId::Tui));
        assert!(viewers.contains(&ClientId::Browser("xyz".to_string())));
    }

    #[test]
    fn test_remove_agent_viewers_clears_selections() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        registry.select_agent(&ClientId::Tui, Some("agent-123"));

        // Remove agent from viewer index
        registry.remove_agent_viewers("agent-123");

        // Both viewer index and selection should be cleared
        assert_eq!(registry.viewer_count("agent-123"), 0);
        assert_eq!(registry.selected_agent(&ClientId::Tui), None);
    }

    #[test]
    fn test_viewers_of_empty() {
        let registry = ClientRegistry::new();

        // No viewers for non-existent agent
        let viewers: Vec<_> = registry.viewers_of("nonexistent").collect();
        assert!(viewers.is_empty());
    }

    #[test]
    fn test_client_starts_with_no_selection() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        // New clients have no selection
        assert_eq!(registry.selected_agent(&ClientId::Tui), None);
    }
}
