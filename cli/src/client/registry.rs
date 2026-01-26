//! Client registry for managing connected clients.
//!
//! A simple wrapper around `HashMap` for storing and retrieving clients by ID.

// Rust guideline compliant 2026-01

use std::collections::HashMap;

use super::{BrowserClient, Client, ClientId, TuiClient};

/// Registry of all connected clients.
///
/// Provides basic CRUD operations for client storage.
///
/// # Example
///
/// ```ignore
/// let mut registry = ClientRegistry::new();
/// registry.register(Box::new(my_client));
/// if let Some(client) = registry.get(&ClientId::Tui) {
///     // Use client
/// }
/// ```
pub struct ClientRegistry {
    clients: HashMap<ClientId, Box<dyn Client>>,
}

impl ClientRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    /// Registers a new client.
    pub fn register(&mut self, client: Box<dyn Client>) {
        let id = client.id().clone();
        self.clients.insert(id, client);
    }

    /// Unregisters a client by ID.
    ///
    /// Returns the removed client if it existed.
    pub fn unregister(&mut self, id: &ClientId) -> Option<Box<dyn Client>> {
        self.clients.remove(id)
    }

    /// Gets a client by ID (immutable).
    pub fn get(&self, id: &ClientId) -> Option<&dyn Client> {
        self.clients.get(id).map(|c| c.as_ref())
    }

    /// Gets a client by ID (mutable).
    pub fn get_mut(&mut self, id: &ClientId) -> Option<&mut Box<dyn Client>> {
        self.clients.get_mut(id)
    }

    /// Iterates all clients (immutable).
    pub fn iter(&self) -> impl Iterator<Item = (&ClientId, &Box<dyn Client>)> {
        self.clients.iter()
    }

    /// Iterates all clients (mutable).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ClientId, &mut Box<dyn Client>)> {
        self.clients.iter_mut()
    }

    /// Gets all client IDs.
    pub fn client_ids(&self) -> impl Iterator<Item = &ClientId> {
        self.clients.keys()
    }

    /// Returns the number of connected clients.
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Returns `true` if the registry contains no clients.
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Poll all clients for pending requests.
    ///
    /// Processes request channels for TuiClient and all BrowserClients.
    /// Called from Hub's main event loop to drain request queues.
    ///
    /// Without this, `TuiRequest` and `BrowserRequest` messages sent by
    /// TuiRunner and browser input receivers will sit in their channels
    /// forever, causing `blocking_recv()` calls in TuiRunner to deadlock.
    pub fn poll_all_requests(&mut self) {
        for client in self.clients.values_mut() {
            if let Some(any) = client.as_any_mut() {
                if let Some(tui) = any.downcast_mut::<TuiClient>() {
                    tui.poll_requests();
                } else if let Some(browser) = any.downcast_mut::<BrowserClient>() {
                    browser.poll_requests();
                }
            }
        }
    }

    /// Get the TuiClient if registered.
    ///
    /// Returns a mutable reference to the TuiClient if one is registered.
    /// This allows Hub to access TuiClient-specific methods that aren't on
    /// the Client trait (like `connected_pty()` and `clear_connection()`).
    ///
    /// Uses `Any` for downcasting internally.
    #[must_use]
    pub fn get_tui_mut(&mut self) -> Option<&mut TuiClient> {
        self.clients.get_mut(&ClientId::Tui).and_then(|boxed| {
            boxed.as_any_mut().and_then(|any| any.downcast_mut::<TuiClient>())
        })
    }

    /// Get the TuiClient if registered (immutable).
    ///
    /// Returns an immutable reference to the TuiClient if one is registered.
    #[must_use]
    pub fn get_tui(&self) -> Option<&TuiClient> {
        self.clients.get(&ClientId::Tui).and_then(|boxed| {
            boxed.as_any().and_then(|any| any.downcast_ref::<TuiClient>())
        })
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
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{AgentHandle, HubHandle};

    /// Test client implementing the Client trait.
    ///
    /// Minimal implementation that stores a mock HubHandle and implements
    /// only the required trait methods. Uses trait defaults for the rest.
    struct TestClient {
        hub_handle: HubHandle,
        id: ClientId,
        dims: (u16, u16),
    }

    impl TestClient {
        fn new(id: ClientId) -> Self {
            Self {
                hub_handle: HubHandle::mock(),
                id,
                dims: (80, 24),
            }
        }
    }

    impl Client for TestClient {
        fn hub_handle(&self) -> &HubHandle {
            &self.hub_handle
        }

        fn id(&self) -> &ClientId {
            &self.id
        }

        fn dims(&self) -> (u16, u16) {
            self.dims
        }

        fn set_dims(&mut self, cols: u16, rows: u16) {
            self.dims = (cols, rows);
        }

        fn connect_to_pty_with_handle(
            &mut self,
            _agent_handle: &AgentHandle,
            _agent_index: usize,
            _pty_index: usize,
        ) -> Result<(), String> {
            Ok(())
        }

        fn disconnect_from_pty(&mut self, _agent_index: usize, _pty_index: usize) {}

        // NOTE: get_agents, get_agent, send_input, resize_pty, agent_count
        // all use DEFAULT IMPLEMENTATIONS from the trait
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
    fn test_unregister() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        let removed = registry.unregister(&ClientId::Tui);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_iter() {
        let mut registry = ClientRegistry::new();

        let tui = TestClient::new(ClientId::Tui);
        let browser = TestClient::new(ClientId::browser("xyz"));

        registry.register(Box::new(tui));
        registry.register(Box::new(browser));

        assert_eq!(registry.iter().count(), 2);
    }

    #[test]
    fn test_client_ids() {
        let mut registry = ClientRegistry::new();
        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));

        let ids: Vec<_> = registry.client_ids().collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], &ClientId::Tui);
    }

    #[test]
    fn test_is_empty() {
        let mut registry = ClientRegistry::new();
        assert!(registry.is_empty());

        let client = TestClient::new(ClientId::Tui);
        registry.register(Box::new(client));
        assert!(!registry.is_empty());
    }
}
