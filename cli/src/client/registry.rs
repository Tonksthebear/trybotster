//! Client task registry for managing async client tasks.
//!
//! A thin HashMap wrapper that stores task handles. Hub shuts down client
//! tasks by aborting their join handles. No business logic, no cached state.

// Rust guideline compliant 2026-01

use std::collections::HashMap;
use tokio::task::JoinHandle;

use super::ClientId;

/// Handle to a spawned async client task.
///
/// Hub holds these to manage client task lifecycle. Tasks are shut down
/// by aborting their join handle. Hub-to-client communication flows via
/// HubEvent broadcast.
#[derive(Debug)]
pub struct ClientTaskHandle {
    /// Async task join handle.
    pub join_handle: JoinHandle<()>,
}

/// Registry of active client task handles.
///
/// A thin HashMap wrapper. Business logic belongs on the Client trait,
/// not here. Callers access handles directly via `get()` and `iter()`.
pub struct ClientRegistry {
    handles: HashMap<ClientId, ClientTaskHandle>,
}

impl ClientRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self { handles: HashMap::new() }
    }

    /// Register a client task handle.
    ///
    /// If a client with the same ID already exists (e.g., browser refresh),
    /// the old task is aborted before registering the new one.
    pub fn register(&mut self, id: ClientId, handle: ClientTaskHandle) {
        if let Some(old) = self.handles.insert(id, handle) {
            old.join_handle.abort();
            log::info!("Replaced existing client task (browser reconnect)");
        }
    }

    /// Unregister and return the client task handle.
    ///
    /// The caller can abort the returned handle's join_handle to stop the task.
    pub fn unregister(&mut self, id: &ClientId) -> Option<ClientTaskHandle> {
        self.handles.remove(id)
    }

    /// Check if a client is registered.
    pub fn contains(&self, id: &ClientId) -> bool {
        self.handles.contains_key(id)
    }

    /// Get a reference to a client task handle.
    pub fn get(&self, id: &ClientId) -> Option<&ClientTaskHandle> {
        self.handles.get(id)
    }

    /// Iterate over all registered client handles.
    pub fn iter(&self) -> impl Iterator<Item = (&ClientId, &ClientTaskHandle)> {
        self.handles.iter()
    }

    /// Get all registered client IDs.
    pub fn client_ids(&self) -> impl Iterator<Item = &ClientId> {
        self.handles.keys()
    }

    /// Returns the number of registered clients.
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    /// Returns true if no clients are registered.
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Shutdown all client tasks.
    ///
    /// Drains all handles and aborts their tasks.
    pub fn shutdown_all(&mut self) {
        for (id, handle) in self.handles.drain() {
            handle.join_handle.abort();
            log::info!("Shut down client {:?}", id);
        }
    }
}

impl Default for ClientRegistry {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Debug for ClientRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRegistry")
            .field("client_count", &self.handles.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Register a test client with a no-op async task.
    fn register_test_client(registry: &mut ClientRegistry, id: ClientId) {
        let join_handle = tokio::spawn(async {});
        registry.register(id, ClientTaskHandle { join_handle });
    }

    #[tokio::test]
    async fn test_register_and_contains() {
        let mut registry = ClientRegistry::new();
        register_test_client(&mut registry, ClientId::Tui);

        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&ClientId::Tui));
        assert!(!registry.contains(&ClientId::Browser("xyz".to_string())));
    }

    #[tokio::test]
    async fn test_unregister() {
        let mut registry = ClientRegistry::new();
        register_test_client(&mut registry, ClientId::Tui);

        let removed = registry.unregister(&ClientId::Tui);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn test_get() {
        let mut registry = ClientRegistry::new();
        register_test_client(&mut registry, ClientId::Tui);

        let handle = registry.get(&ClientId::Tui);
        assert!(handle.is_some());
        assert!(registry.get(&ClientId::Browser("xyz".to_string())).is_none());
    }

    #[tokio::test]
    async fn test_iter() {
        let mut registry = ClientRegistry::new();
        register_test_client(&mut registry, ClientId::Tui);
        register_test_client(&mut registry, ClientId::browser("xyz"));

        let entries: Vec<_> = registry.iter().collect();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_client_ids() {
        let mut registry = ClientRegistry::new();
        register_test_client(&mut registry, ClientId::Tui);

        let ids: Vec<_> = registry.client_ids().collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], &ClientId::Tui);
    }

    #[tokio::test]
    async fn test_is_empty() {
        let mut registry = ClientRegistry::new();
        assert!(registry.is_empty());

        register_test_client(&mut registry, ClientId::Tui);
        assert!(!registry.is_empty());
    }

    #[tokio::test]
    async fn test_shutdown_all() {
        let mut registry = ClientRegistry::new();
        register_test_client(&mut registry, ClientId::Tui);
        register_test_client(&mut registry, ClientId::browser("xyz"));

        registry.shutdown_all();

        assert!(registry.is_empty());
    }
}
