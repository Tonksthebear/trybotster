//! Connected client tracking for PTY sessions.
//!
//! This module provides the [`ConnectedClient`] struct that PTY sessions use
//! to track which clients are connected. The newest connected client becomes
//! the "size owner" whose terminal dimensions are applied to the PTY.
//!
//! # Size Ownership
//!
//! - Newest client (most recent `connected_at`) owns the PTY size
//! - When the owner disconnects, ownership passes to the next most recent
//! - When no clients are connected, the PTY keeps its current size
//!
//! # Usage
//!
//! ```ignore
//! let client = ConnectedClient::new(ClientId::Tui, (80, 24));
//! println!("Connected at: {:?}", client.connected_at);
//! ```

// Rust guideline compliant 2026-01

use std::time::Instant;

use crate::client::ClientId;

/// A client connected to a PTY session.
///
/// Tracks the client's identity, terminal dimensions, and connection time.
/// The PTY session maintains a list of these, ordered by connection time,
/// with the newest client being the size owner.
#[derive(Debug, Clone)]
pub struct ConnectedClient {
    /// Unique identifier for this client.
    pub id: ClientId,

    /// Terminal dimensions (cols, rows).
    pub dims: (u16, u16),

    /// When this client connected.
    ///
    /// Used to determine size ownership - newest client owns the size.
    pub connected_at: Instant,
}

impl ConnectedClient {
    /// Create a new connected client record.
    ///
    /// The connection time is set to now.
    #[must_use]
    pub fn new(id: ClientId, dims: (u16, u16)) -> Self {
        Self {
            id,
            dims,
            connected_at: Instant::now(),
        }
    }

    /// Create a connected client with a specific connection time.
    ///
    /// Primarily for testing.
    #[must_use]
    pub fn with_connected_at(id: ClientId, dims: (u16, u16), connected_at: Instant) -> Self {
        Self {
            id,
            dims,
            connected_at,
        }
    }

    /// Get the terminal width in columns.
    #[must_use]
    pub fn cols(&self) -> u16 {
        self.dims.0
    }

    /// Get the terminal height in rows.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.dims.1
    }

    /// Update the terminal dimensions.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
    }
}

impl PartialEq for ConnectedClient {
    fn eq(&self, other: &Self) -> bool {
        // Compare by ID only - dims and time can differ
        self.id == other.id
    }
}

impl Eq for ConnectedClient {}

impl std::hash::Hash for ConnectedClient {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_connected_client_new() {
        let client = ConnectedClient::new(ClientId::Tui, (80, 24));

        assert_eq!(client.id, ClientId::Tui);
        assert_eq!(client.dims, (80, 24));
        assert_eq!(client.cols(), 80);
        assert_eq!(client.rows(), 24);
    }

    #[test]
    fn test_connected_client_browser() {
        let client = ConnectedClient::new(ClientId::browser("abc123"), (120, 40));

        assert!(client.id.is_browser());
        assert_eq!(client.cols(), 120);
        assert_eq!(client.rows(), 40);
    }

    #[test]
    fn test_connected_client_resize() {
        let mut client = ConnectedClient::new(ClientId::Tui, (80, 24));

        client.resize(100, 30);

        assert_eq!(client.dims, (100, 30));
        assert_eq!(client.cols(), 100);
        assert_eq!(client.rows(), 30);
    }

    #[test]
    fn test_connected_client_with_connected_at() {
        let earlier = Instant::now();
        std::thread::sleep(Duration::from_millis(10));

        let client = ConnectedClient::with_connected_at(ClientId::Tui, (80, 24), earlier);

        assert!(client.connected_at < Instant::now());
    }

    #[test]
    fn test_connected_client_equality_by_id() {
        let client1 = ConnectedClient::new(ClientId::Tui, (80, 24));
        let client2 = ConnectedClient::new(ClientId::Tui, (100, 30)); // Different dims

        // Same ID = equal
        assert_eq!(client1, client2);

        let client3 = ConnectedClient::new(ClientId::browser("abc"), (80, 24));
        assert_ne!(client1, client3);
    }

    #[test]
    fn test_connected_client_hash_by_id() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(ConnectedClient::new(ClientId::Tui, (80, 24)));

        // Same ID with different dims should not be inserted again
        let duplicate = ConnectedClient::new(ClientId::Tui, (100, 30));
        assert!(!set.insert(duplicate));

        // Different ID should be inserted
        let different = ConnectedClient::new(ClientId::browser("abc"), (80, 24));
        assert!(set.insert(different));
    }

    #[test]
    fn test_connected_client_ordering_by_time() {
        let earlier = Instant::now();
        std::thread::sleep(Duration::from_millis(10));
        let later = Instant::now();

        let client1 = ConnectedClient::with_connected_at(ClientId::Tui, (80, 24), earlier);
        let client2 = ConnectedClient::with_connected_at(ClientId::browser("abc"), (80, 24), later);

        assert!(client1.connected_at < client2.connected_at);
    }

    #[test]
    fn test_connected_client_clone() {
        let client = ConnectedClient::new(ClientId::Tui, (80, 24));
        let cloned = client.clone();

        assert_eq!(client.id, cloned.id);
        assert_eq!(client.dims, cloned.dims);
    }

    #[test]
    fn test_connected_client_debug() {
        let client = ConnectedClient::new(ClientId::Tui, (80, 24));
        let debug = format!("{:?}", client);

        assert!(debug.contains("ConnectedClient"));
        assert!(debug.contains("Tui"));
        assert!(debug.contains("80"));
        assert!(debug.contains("24"));
    }
}
