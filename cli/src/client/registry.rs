//! Client task registry for managing async client tasks.
//!
//! A thin HashMap wrapper that stores task handles. Hub communicates with
//! clients via `ClientCmd` channels. No business logic, no cached state.

// Rust guideline compliant 2026-01

use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::{ClientCmd, ClientId};

/// Handle to a spawned async client task.
///
/// Hub holds these to communicate with clients via channels.
/// When dropped, the command channel closes, signaling the client task to exit.
#[derive(Debug)]
pub struct ClientTaskHandle {
    /// Channel for Hub -> Client commands.
    pub cmd_tx: mpsc::Sender<ClientCmd>,
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
    /// the old task is shut down before registering the new one.
    pub fn register(&mut self, id: ClientId, handle: ClientTaskHandle) {
        if let Some(old) = self.handles.insert(id, handle) {
            let _ = old.cmd_tx.try_send(ClientCmd::Shutdown);
            old.join_handle.abort();
            log::info!("Replaced existing client task (browser reconnect)");
        }
    }

    /// Unregister and return the client task handle.
    ///
    /// Dropping the handle closes the command channel, signaling shutdown.
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
    /// Drains all handles and aborts their tasks. Dropping `cmd_tx` signals
    /// the client's `run_task` to exit (it sees `None` from `cmd_rx.recv()`).
    /// Abort is the safety net for tasks that don't check the channel.
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

    /// Register a test client and return the command receiver for verification.
    fn register_test_client(registry: &mut ClientRegistry, id: ClientId) -> mpsc::Receiver<ClientCmd> {
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let join_handle = tokio::spawn(async {});
        registry.register(id, ClientTaskHandle { cmd_tx, join_handle });
        cmd_rx
    }

    #[tokio::test]
    async fn test_register_and_contains() {
        let mut registry = ClientRegistry::new();
        let _rx = register_test_client(&mut registry, ClientId::Tui);

        assert_eq!(registry.len(), 1);
        assert!(registry.contains(&ClientId::Tui));
        assert!(!registry.contains(&ClientId::Browser("xyz".to_string())));
    }

    #[tokio::test]
    async fn test_unregister() {
        let mut registry = ClientRegistry::new();
        let _rx = register_test_client(&mut registry, ClientId::Tui);

        let removed = registry.unregister(&ClientId::Tui);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn test_get() {
        let mut registry = ClientRegistry::new();
        let _rx = register_test_client(&mut registry, ClientId::Tui);

        let handle = registry.get(&ClientId::Tui);
        assert!(handle.is_some());
        assert!(registry.get(&ClientId::Browser("xyz".to_string())).is_none());
    }

    #[tokio::test]
    async fn test_iter() {
        let mut registry = ClientRegistry::new();
        let _rx1 = register_test_client(&mut registry, ClientId::Tui);
        let _rx2 = register_test_client(&mut registry, ClientId::browser("xyz"));

        let entries: Vec<_> = registry.iter().collect();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_send_via_get() {
        let mut registry = ClientRegistry::new();
        let mut rx = register_test_client(&mut registry, ClientId::Tui);

        // Callers use get() + try_send() directly
        if let Some(handle) = registry.get(&ClientId::Tui) {
            let _ = handle.cmd_tx.try_send(ClientCmd::Shutdown);
        }

        let cmd = rx.try_recv().unwrap();
        assert!(matches!(cmd, ClientCmd::Shutdown));
    }

    #[tokio::test]
    async fn test_broadcast_via_iter() {
        let mut registry = ClientRegistry::new();
        let mut rx1 = register_test_client(&mut registry, ClientId::Tui);
        let mut rx2 = register_test_client(&mut registry, ClientId::browser("xyz"));

        // Callers iterate and send directly
        for (_, handle) in registry.iter() {
            let _ = handle.cmd_tx.try_send(ClientCmd::Shutdown);
        }

        assert!(matches!(rx1.try_recv().unwrap(), ClientCmd::Shutdown));
        assert!(matches!(rx2.try_recv().unwrap(), ClientCmd::Shutdown));
    }

    #[tokio::test]
    async fn test_client_ids() {
        let mut registry = ClientRegistry::new();
        let _rx = register_test_client(&mut registry, ClientId::Tui);

        let ids: Vec<_> = registry.client_ids().collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], &ClientId::Tui);
    }

    #[tokio::test]
    async fn test_is_empty() {
        let mut registry = ClientRegistry::new();
        assert!(registry.is_empty());

        let _rx = register_test_client(&mut registry, ClientId::Tui);
        assert!(!registry.is_empty());
    }

    #[tokio::test]
    async fn test_shutdown_all() {
        let mut registry = ClientRegistry::new();
        let _rx1 = register_test_client(&mut registry, ClientId::Tui);
        let _rx2 = register_test_client(&mut registry, ClientId::browser("xyz"));

        registry.shutdown_all();

        assert!(registry.is_empty());
        // cmd_tx dropped during drain, so receivers will see channel closed
    }
}
