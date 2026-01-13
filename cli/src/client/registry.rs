//! Client registry with optimized viewer tracking.
//!
//! The registry maintains all connected clients and provides O(1) lookup
//! for PTY output routing via a reverse index.

use std::collections::{HashMap, HashSet};

use super::{Client, ClientId};

/// Registry of all connected clients with reverse index for PTY routing.
///
/// # Reverse Index
///
/// The `viewers` HashMap maps agent keys to sets of client IDs viewing that agent.
/// This enables O(1) lookup when routing PTY output, instead of iterating all clients.
///
/// # Example
///
/// ```text
/// viewers: {
///     "agent-abc": { ClientId::Tui, ClientId::Browser("xyz") },
///     "agent-def": { ClientId::Browser("123") },
/// }
/// ```
///
/// When agent "agent-abc" produces output, we can immediately find all viewers.
pub struct ClientRegistry {
    /// All clients by ID.
    clients: HashMap<ClientId, Box<dyn Client>>,

    /// Reverse index: agent_key -> set of client IDs viewing that agent.
    /// Enables O(1) lookup for PTY output routing.
    viewers: HashMap<String, HashSet<ClientId>>,
}

impl ClientRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            viewers: HashMap::new(),
        }
    }

    /// Register a new client.
    ///
    /// If a client with the same ID already exists, it is replaced.
    pub fn register(&mut self, client: Box<dyn Client>) {
        let id = client.id().clone();

        // If client already has a selection, add to viewer index
        if let Some(agent_key) = client.state().selected_agent.as_ref() {
            self.viewers
                .entry(agent_key.clone())
                .or_default()
                .insert(id.clone());
        }

        self.clients.insert(id, client);
    }

    /// Unregister a client, cleaning up viewer index.
    ///
    /// Returns the removed client if it existed.
    pub fn unregister(&mut self, id: &ClientId) -> Option<Box<dyn Client>> {
        if let Some(client) = self.clients.remove(id) {
            // Remove from viewer index
            if let Some(agent_key) = client.state().selected_agent.as_ref() {
                if let Some(viewers) = self.viewers.get_mut(agent_key) {
                    viewers.remove(id);
                    if viewers.is_empty() {
                        self.viewers.remove(agent_key);
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

    /// Update viewer index when client changes selection.
    ///
    /// This must be called BEFORE updating the client's state to maintain consistency.
    ///
    /// # Arguments
    ///
    /// * `client_id` - The client changing selection
    /// * `old_agent` - Previous selected agent (if any)
    /// * `new_agent` - New selected agent (if any)
    pub fn update_selection(
        &mut self,
        client_id: &ClientId,
        old_agent: Option<&str>,
        new_agent: Option<&str>,
    ) {
        // Remove from old agent's viewers
        if let Some(old_key) = old_agent {
            if let Some(viewers) = self.viewers.get_mut(old_key) {
                viewers.remove(client_id);
                if viewers.is_empty() {
                    self.viewers.remove(old_key);
                }
            }
        }

        // Add to new agent's viewers
        if let Some(new_key) = new_agent {
            self.viewers
                .entry(new_key.to_string())
                .or_default()
                .insert(client_id.clone());
        }
    }

    /// Get all client IDs viewing a specific agent (O(1) lookup).
    ///
    /// Returns an empty iterator if no clients are viewing the agent.
    pub fn viewers_of(&self, agent_key: &str) -> impl Iterator<Item = &ClientId> {
        self.viewers
            .get(agent_key)
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
    /// Note: This does NOT clear clients' selected_agent state.
    /// The caller should separately call clear_selection on affected clients.
    pub fn remove_agent_viewers(&mut self, agent_key: &str) {
        self.viewers.remove(agent_key);
    }

    /// Flush all clients (for batched output).
    ///
    /// Called at end of event loop iteration to send batched output.
    pub fn flush_all(&mut self) {
        for client in self.clients.values_mut() {
            client.flush();
        }
    }

    /// Get count of viewers for a specific agent.
    pub fn viewer_count(&self, agent_key: &str) -> usize {
        self.viewers
            .get(agent_key)
            .map(|set| set.len())
            .unwrap_or(0)
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
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{AgentInfo, ClientState, Response, WorktreeInfo};

    /// Test client for unit tests.
    struct TestClient {
        id: ClientId,
        state: ClientState,
        output_received: Vec<u8>,
    }

    impl TestClient {
        fn new(id: ClientId) -> Self {
            Self {
                id,
                state: ClientState::default(),
                output_received: Vec::new(),
            }
        }

        fn with_selection(mut self, agent_key: &str) -> Self {
            self.state.selected_agent = Some(agent_key.to_string());
            self
        }
    }

    impl Client for TestClient {
        fn id(&self) -> &ClientId {
            &self.id
        }

        fn state(&self) -> &ClientState {
            &self.state
        }

        fn state_mut(&mut self) -> &mut ClientState {
            &mut self.state
        }

        fn receive_output(&mut self, data: &[u8]) {
            self.output_received.extend_from_slice(data);
        }

        fn receive_scrollback(&mut self, _lines: Vec<String>) {}

        fn receive_agent_list(&mut self, _agents: Vec<AgentInfo>) {}

        fn receive_worktree_list(&mut self, _worktrees: Vec<WorktreeInfo>) {}

        fn receive_response(&mut self, _response: Response) {}
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        assert_eq!(registry.len(), 1);
        assert!(registry.get(&ClientId::Tui).is_some());
        assert!(registry.get(&ClientId::Browser("xyz".to_string())).is_none());
    }

    #[test]
    fn test_unregister() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui).with_selection("agent-123");
        registry.register(Box::new(client));

        // Verify viewer index populated
        assert_eq!(registry.viewer_count("agent-123"), 1);

        // Unregister
        let removed = registry.unregister(&ClientId::Tui);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
        assert_eq!(registry.viewer_count("agent-123"), 0);
    }

    #[test]
    fn test_viewer_index_on_register() {
        let mut registry = ClientRegistry::new();

        // Register client already viewing an agent
        let client = TestClient::new(ClientId::Tui).with_selection("agent-abc");
        registry.register(Box::new(client));

        // Should be in viewer index
        let viewers: Vec<_> = registry.viewers_of("agent-abc").collect();
        assert_eq!(viewers.len(), 1);
        assert_eq!(viewers[0], &ClientId::Tui);
    }

    #[test]
    fn test_update_selection() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui).with_selection("agent-old");
        registry.register(Box::new(client));

        // Update selection
        registry.update_selection(&ClientId::Tui, Some("agent-old"), Some("agent-new"));

        // Update client state (caller responsibility)
        if let Some(client) = registry.get_mut(&ClientId::Tui) {
            client.select_agent("agent-new");
        }

        // Verify viewer index updated
        assert_eq!(registry.viewer_count("agent-old"), 0);
        assert_eq!(registry.viewer_count("agent-new"), 1);
    }

    #[test]
    fn test_multiple_viewers() {
        let mut registry = ClientRegistry::new();

        // Two clients viewing same agent
        let tui = TestClient::new(ClientId::Tui).with_selection("agent-shared");
        let browser = TestClient::new(ClientId::browser("xyz")).with_selection("agent-shared");

        registry.register(Box::new(tui));
        registry.register(Box::new(browser));

        assert_eq!(registry.viewer_count("agent-shared"), 2);

        let viewers: HashSet<_> = registry.viewers_of("agent-shared").cloned().collect();
        assert!(viewers.contains(&ClientId::Tui));
        assert!(viewers.contains(&ClientId::Browser("xyz".to_string())));
    }

    #[test]
    fn test_remove_agent_viewers() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui).with_selection("agent-123");
        registry.register(Box::new(client));

        // Remove agent from viewer index
        registry.remove_agent_viewers("agent-123");

        // Viewer index cleared
        assert_eq!(registry.viewer_count("agent-123"), 0);

        // But client still has selection (caller must clear separately)
        let state = registry.get(&ClientId::Tui).unwrap().state();
        assert_eq!(state.selected_agent, Some("agent-123".to_string()));
    }

    #[test]
    fn test_viewers_of_empty() {
        let registry = ClientRegistry::new();

        // No viewers for non-existent agent
        let viewers: Vec<_> = registry.viewers_of("nonexistent").collect();
        assert!(viewers.is_empty());
    }
}
