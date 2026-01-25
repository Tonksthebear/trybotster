//! TUI client implementation for the local terminal interface.
//!
//! `TuiClient` represents the local terminal user. Unlike browser clients which
//! communicate over WebSockets, the TUI client directly owns its terminal state
//! and interacts with the Hub/PTY through handles and tokio channels.
//!
//! # Architecture
//!
//! ```text
//! TuiClient
//!   ├── hub_handle (required - thread-safe access to Hub state and commands)
//!   ├── id (always ClientId::Tui)
//!   ├── dims (cols, rows)
//!   ├── vt100_parser (owns terminal emulation state)
//!   └── pty_event_rx (subscribed to currently-displayed PTY events)
//! ```
//!
//! # Event Flow
//!
//! 1. PTY emits `PtyEvent::Output` -> TuiClient polls via `poll_pty_events()`
//!    -> TuiRunner calls `process_output()` to feed vt100_parser
//! 2. User types -> TuiRunner calls trait's `send_input()` via hub_handle lookup
//! 3. Hub emits events -> TuiRunner polls Hub directly, not through TuiClient
//!
//! # Minimal Design
//!
//! TuiClient is intentionally minimal:
//! - NO selection tracking (TuiRunner owns `selected_agent`, `active_pty_view`)
//! - NO pty_handles storage (uses hub_handle lookup for each operation)
//! - NO hub_event_rx (TuiRunner subscribes directly to Hub events)
//!
//! The trait's default implementations handle `get_agents`, `get_agent`,
//! `send_input`, `resize_pty`, and `agent_count` via hub_handle lookup.

// Rust guideline compliant 2026-01

use std::sync::{Arc, Mutex};

use log::warn;
use tokio::sync::broadcast;
use vt100::Parser;

use crate::agent::pty::PtyEvent;
use crate::hub::hub_handle::HubHandle;

use super::{Client, ClientId};

/// TUI client - the local terminal interface.
///
/// Minimal implementation that stores only what's required:
/// - Hub access via `hub_handle`
/// - Client identity (`id`)
/// - Terminal dimensions (`dims`)
/// - Terminal emulation (`vt100_parser`)
/// - PTY event subscription (`pty_event_rx`)
///
/// # What's NOT Here (GUI State in TuiRunner)
///
/// - Current agent/PTY selection for display (TuiRunner tracks this)
/// - Scroll position (TuiRunner manages)
/// - Which PTY is "active" for viewing (TuiRunner manages)
/// - PTY handles storage (uses hub_handle lookup instead)
pub struct TuiClient {
    /// Thread-safe access to Hub state and operations.
    hub_handle: HubHandle,

    /// Unique identifier (always `ClientId::Tui`).
    id: ClientId,

    /// Terminal dimensions (cols, rows).
    dims: (u16, u16),

    /// Terminal emulator for processing PTY output.
    ///
    /// Shared with the render loop for screen access.
    vt100_parser: Arc<Mutex<Parser>>,

    /// Receiver for PTY events from the currently-displayed PTY.
    ///
    /// `None` when not subscribed to any PTY for display.
    /// Set by `connect_to_pty()`, cleared by `disconnect_from_pty()`.
    pty_event_rx: Option<broadcast::Receiver<PtyEvent>>,

    /// Currently connected PTY indices, if any.
    ///
    /// Stores (agent_index, pty_index) when connected to a PTY.
    /// Used by `set_dims()` to propagate resize to the connected PTY.
    connected_pty: Option<(usize, usize)>,
}

impl TuiClient {
    /// Create a new TUI client with default dimensions.
    #[must_use]
    pub fn new(hub_handle: HubHandle) -> Self {
        Self::with_dims(hub_handle, 80, 24)
    }

    /// Create a new TUI client with specific dimensions.
    #[must_use]
    pub fn with_dims(hub_handle: HubHandle, cols: u16, rows: u16) -> Self {
        Self {
            hub_handle,
            id: ClientId::Tui,
            dims: (cols, rows),
            // Magic value 10000: scrollback buffer size, large enough for typical
            // terminal sessions. Too small loses history, too large wastes memory.
            vt100_parser: Arc::new(Mutex::new(Parser::new(rows, cols, 10000))),
            pty_event_rx: None,
            connected_pty: None,
        }
    }

    /// Create a TUI client with an existing vt100 parser.
    ///
    /// Used when the parser needs to be shared with the render loop.
    #[must_use]
    pub fn with_parser(
        hub_handle: HubHandle,
        parser: Arc<Mutex<Parser>>,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            hub_handle,
            id: ClientId::Tui,
            dims: (cols, rows),
            vt100_parser: parser,
            pty_event_rx: None,
            connected_pty: None,
        }
    }

    // === Helper methods (not part of trait) ===

    /// Poll for PTY events (non-blocking).
    ///
    /// Returns the next PTY event if available, or `None` if no events pending.
    /// Returns `Err` if the channel is closed (agent terminated).
    ///
    /// Called by TuiRunner in the event loop to check for PTY output.
    pub fn poll_pty_events(&mut self) -> Result<Option<PtyEvent>, broadcast::error::RecvError> {
        match &mut self.pty_event_rx {
            Some(rx) => match rx.try_recv() {
                Ok(event) => Ok(Some(event)),
                Err(broadcast::error::TryRecvError::Empty) => Ok(None),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    // We missed some events, continue receiving
                    warn!("TUI lagged behind PTY by {} events", n);
                    Ok(None)
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    Err(broadcast::error::RecvError::Closed)
                }
            },
            None => Ok(None),
        }
    }

    /// Update local dimensions.
    ///
    /// Also resizes the vt100 parser. Does NOT automatically resize PTYs -
    /// caller should use trait's `resize_pty()` for specific PTYs that should be resized.
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);

        // Update parser
        if let Ok(mut parser) = self.vt100_parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    /// Get the vt100 parser for rendering.
    #[must_use]
    pub fn vt100_parser(&self) -> &Arc<Mutex<Parser>> {
        &self.vt100_parser
    }

    /// Process PTY output by feeding to vt100 parser.
    ///
    /// Called by TuiRunner when `poll_pty_events()` returns `PtyEvent::Output`.
    /// Separated from polling so TuiRunner controls when parsing happens.
    pub fn process_output(&mut self, data: &[u8]) {
        if let Ok(mut parser) = self.vt100_parser.lock() {
            parser.process(data);
        }
    }
}

impl std::fmt::Debug for TuiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiClient")
            .field("id", &self.id)
            .field("dims", &self.dims)
            .field("has_pty_event_rx", &self.pty_event_rx.is_some())
            .finish()
    }
}

impl Client for TuiClient {
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
        self.update_dims(cols, rows);

        // Propagate resize to connected PTY
        if let Some((agent_idx, pty_idx)) = self.connected_pty {
            if let Err(e) = self.resize_pty(agent_idx, pty_idx, rows, cols) {
                log::debug!("Failed to resize PTY: {}", e);
            }
        }
    }

    fn connect_to_pty(&mut self, agent_index: usize, pty_index: usize) -> Result<(), String> {
        // Disconnect from previous if any
        if self.pty_event_rx.is_some() {
            self.pty_event_rx = None;
            self.connected_pty = None;
        }

        // Get PTY handle via hub_handle and subscribe
        let agent = self
            .hub_handle
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;
        let pty = agent
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found", pty_index))?;

        self.pty_event_rx = Some(pty.subscribe());

        // Track connected PTY indices for resize propagation
        self.connected_pty = Some((agent_index, pty_index));

        // Notify PTY of connection and get scrollback (currently unused)
        let _scrollback = pty.connect_blocking(self.id.clone(), self.dims);

        Ok(())
    }

    fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        self.pty_event_rx = None;
        self.connected_pty = None;

        // Notify PTY of disconnection
        if let Some(agent) = self.hub_handle.get_agent(agent_index) {
            if let Some(pty) = agent.get_pty(pty_index) {
                let _ = pty.disconnect_blocking(self.id.clone());
            }
        }
    }

    // NOTE: get_agents, get_agent, send_input, resize_pty, agent_count
    // all use DEFAULT IMPLEMENTATIONS from the trait - not implemented here
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a TuiClient with a mock HubHandle for testing.
    fn test_client() -> TuiClient {
        TuiClient::new(HubHandle::mock())
    }

    /// Helper to create a TuiClient with specific dimensions.
    fn test_client_with_dims(cols: u16, rows: u16) -> TuiClient {
        TuiClient::with_dims(HubHandle::mock(), cols, rows)
    }

    #[test]
    fn test_construction_default() {
        let client = test_client();
        assert_eq!(client.id(), &ClientId::Tui);
        assert_eq!(client.dims(), (80, 24));
    }

    #[test]
    fn test_construction_with_dims() {
        let client = test_client_with_dims(120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_construction_with_parser() {
        let parser = Arc::new(Mutex::new(Parser::new(30, 100, 5000)));
        let client = TuiClient::with_parser(HubHandle::mock(), parser.clone(), 100, 30);

        // Verify same parser is shared
        assert!(Arc::ptr_eq(client.vt100_parser(), &parser));
        assert_eq!(client.dims(), (100, 30));
    }

    #[test]
    fn test_dims_accessor() {
        let client = test_client_with_dims(100, 50);
        assert_eq!(client.dims(), (100, 50));
    }

    #[test]
    fn test_vt100_parser_accessor() {
        let client = test_client();
        let parser = client.vt100_parser();
        // Verify we can access the parser
        let lock = parser.lock().unwrap();
        assert_eq!(lock.screen().size(), (24, 80));
    }

    #[test]
    fn test_update_dims() {
        let mut client = test_client();

        client.update_dims(100, 30);
        assert_eq!(client.dims(), (100, 30));

        // Verify parser was resized
        let parser = client.vt100_parser().lock().unwrap();
        assert_eq!(parser.screen().size(), (30, 100));
    }

    #[test]
    fn test_process_output() {
        let mut client = test_client();

        client.process_output(b"Hello, World!");

        let parser = client.vt100_parser().lock().unwrap();
        let contents = parser.screen().contents();
        assert!(contents.contains("Hello, World!"));
    }

    #[test]
    fn test_poll_pty_events_without_subscription() {
        let mut client = test_client();
        let result = client.poll_pty_events();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_poll_pty_events_with_subscription() {
        // Set up channel and manually assign receiver
        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);

        let mut client = test_client();
        // Directly set the receiver for testing
        client.pty_event_rx = Some(event_tx.subscribe());

        // Send an event
        event_tx.send(PtyEvent::output(b"hello".to_vec())).unwrap();

        // Poll should receive it
        let result = client.poll_pty_events();
        assert!(result.is_ok());
        let event = result.unwrap();
        assert!(event.is_some());

        match event.unwrap() {
            PtyEvent::Output(data) => assert_eq!(data, b"hello"),
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_connect_to_pty_fails_with_mock() {
        // With mock hub_handle, connect_to_pty will fail because
        // get_agent returns None.
        let mut client = test_client();

        let result = client.connect_to_pty(0, 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_disconnect_from_pty_safe_when_not_connected() {
        let mut client = test_client();

        // Disconnect is always safe (no-op when not connected)
        client.disconnect_from_pty(0, 0);
        // Should not panic
    }

    #[test]
    fn test_trait_hub_handle_accessor() {
        let client = test_client();
        // Verify hub_handle() returns a reference (trait method)
        let _ = client.hub_handle();
    }

    #[test]
    fn test_trait_default_get_agents() {
        let client = test_client();
        // Mock hub_handle returns empty vec
        let agents = Client::get_agents(&client);
        assert!(agents.is_empty());
    }

    #[test]
    fn test_trait_default_get_agent() {
        let client = test_client();
        // Mock hub_handle returns None
        let agent = Client::get_agent(&client, 0);
        assert!(agent.is_none());
    }

    #[test]
    fn test_trait_default_send_input_fails_without_agent() {
        let client = test_client();
        // Default implementation looks up via hub_handle, which returns None
        let result = Client::send_input(&client, 0, 0, b"test");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_trait_default_resize_pty_fails_without_agent() {
        let client = test_client();
        // Default implementation looks up via hub_handle, which returns None
        let result = Client::resize_pty(&client, 0, 0, 24, 80);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_trait_default_agent_count() {
        let client = test_client();
        // Mock returns empty, so count is 0
        assert_eq!(Client::agent_count(&client), 0);
    }

    #[test]
    fn test_debug_format() {
        let client = test_client();
        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("TuiClient"));
        assert!(debug_str.contains("id"));
        assert!(debug_str.contains("dims"));
    }
}
