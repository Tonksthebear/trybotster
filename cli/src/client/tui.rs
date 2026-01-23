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
//!   ├── vt100_parser (owns terminal emulation state)
//!   ├── pty_event_rx (subscribed to PTY events via PtyHandle)
//!   ├── hub_handle (required - thread-safe access to Hub state and commands)
//!   └── hub_event_rx (receives Hub events)
//! ```
//!
//! # Event Flow
//!
//! 1. PTY emits `PtyEvent::Output` → TuiClient receives → feeds to vt100_parser
//! 2. User types → TuiClient calls `send_input()` via PtyHandle
//! 3. Hub emits `HubEvent::AgentCreated` → TuiClient receives → updates UI state

// Rust guideline compliant 2026-01

use std::sync::{Arc, Mutex};

use log::warn;
use tokio::sync::broadcast;

use crate::agent::pty::PtyEvent;
use crate::agent::PtyView;
use crate::hub::agent_handle::{AgentHandle, PtyHandle};
use crate::hub::commands::CreateAgentRequest as HubCreateAgentRequest;
use crate::hub::events::HubEvent;
use crate::hub::hub_handle::HubHandle;
use crate::relay::AgentInfo;

use super::types::CreateAgentRequest;

use super::ClientId;

/// TUI client - the local terminal interface.
///
/// Owns UI state and terminal emulation. Implements the Client trait for
/// Hub/PTY interaction while maintaining its own view state for the TUI.
pub struct TuiClient {
    /// Unique identifier (always `ClientId::Tui`).
    id: ClientId,

    /// Terminal dimensions (cols, rows).
    dims: (u16, u16),

    /// Terminal emulator for processing PTY output.
    ///
    /// Shared with the render loop for screen access.
    vt100_parser: Arc<Mutex<vt100::Parser>>,

    /// Whether this client owns the PTY size.
    ///
    /// The most recently connected client becomes the size owner.
    /// When multiple clients are connected, only the owner's resize
    /// events affect the actual PTY dimensions.
    is_size_owner: bool,

    // === UI State (TuiClient's own, not on Client trait) ===
    /// Currently selected agent in the agent list.
    selected_agent: Option<String>,

    /// Which PTY view is active (CLI or Server).
    active_pty_view: PtyView,

    /// Which agent's PTY we're subscribed to.
    ///
    /// Different from `selected_agent` - this tracks the actual PTY
    /// subscription. Selection can change before connection.
    connected_agent: Option<String>,

    // === Channels ===
    /// Receiver for PTY events from the connected agent.
    ///
    /// `None` when not connected to any agent's PTY.
    pty_event_rx: Option<broadcast::Receiver<PtyEvent>>,

    /// Receiver for Hub-level events.
    ///
    /// Receives agent created/deleted/status events.
    hub_event_rx: Option<broadcast::Receiver<HubEvent>>,

    /// Current PTY handle for the connected agent.
    ///
    /// Stored to enable `send_input()` and resize operations.
    current_pty_handle: Option<PtyHandle>,

    /// Handle for Hub communication.
    ///
    /// Used by `get_agents()` and `get_agent()` to query Hub state.
    /// Also used for fire-and-forget create/delete agent commands.
    hub_handle: HubHandle,
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
            id: ClientId::Tui,
            dims: (cols, rows),
            vt100_parser: Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 10000))),
            is_size_owner: true, // TUI starts as owner
            selected_agent: None,
            active_pty_view: PtyView::default(),
            connected_agent: None,
            pty_event_rx: None,
            hub_event_rx: None,
            current_pty_handle: None,
            hub_handle,
        }
    }

    /// Create a TUI client with an existing vt100 parser.
    ///
    /// Used when the parser needs to be shared with the render loop.
    #[must_use]
    pub fn with_parser(
        hub_handle: HubHandle,
        parser: Arc<Mutex<vt100::Parser>>,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            id: ClientId::Tui,
            dims: (cols, rows),
            vt100_parser: parser,
            is_size_owner: true,
            selected_agent: None,
            active_pty_view: PtyView::default(),
            connected_agent: None,
            pty_event_rx: None,
            hub_event_rx: None,
            current_pty_handle: None,
            hub_handle,
        }
    }

    // === Client trait methods ===

    /// Get the client ID.
    #[must_use]
    pub fn id(&self) -> &ClientId {
        &self.id
    }

    /// Get current terminal dimensions (cols, rows).
    #[must_use]
    pub fn dims(&self) -> (u16, u16) {
        self.dims
    }

    /// Handle PTY output by feeding to vt100 parser.
    ///
    /// Called when receiving `PtyEvent::Output`.
    pub fn on_output(&mut self, data: &[u8]) {
        if let Ok(mut parser) = self.vt100_parser.lock() {
            parser.process(data);
        }
    }

    /// Handle PTY resized notification.
    ///
    /// Updates the vt100 parser dimensions to match the new PTY size.
    pub fn on_resized(&mut self, rows: u16, cols: u16) {
        self.dims = (cols, rows);
        if let Ok(mut parser) = self.vt100_parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }
    }

    /// Handle PTY process exit.
    ///
    /// The connected agent's process has terminated.
    pub fn on_process_exit(&mut self, _exit_code: Option<i32>) {
        // The process exited but we remain subscribed to the PTY
        // for any final output. UI can show exit status.
    }

    /// Handle PTY size ownership change.
    ///
    /// Updates whether this client owns the PTY size.
    pub fn on_owner_changed(&mut self, new_owner: Option<ClientId>) {
        self.is_size_owner = new_owner.as_ref() == Some(&self.id);
    }

    /// Handle agent created event from Hub.
    ///
    /// A new agent was created at the given index. UI should update agent list.
    pub fn on_agent_created(&mut self, _index: usize, _info: &AgentInfo) {
        // UI will re-render agent list from Hub state
    }

    /// Handle agent deleted event from Hub.
    ///
    /// An agent at the given index was deleted. If we were connected to it, disconnect.
    /// Note: The index refers to the position before deletion.
    pub fn on_agent_deleted(&mut self, index: usize) {
        // Get the agent_id at this index to check if we need to disconnect
        let agent_id = self.get_agent_id_at_index(index);

        // If we were connected to this agent, clean up
        if let Some(ref aid) = agent_id {
            if self.connected_agent.as_deref() == Some(aid) {
                self.disconnect_from_pty();
            }

            // If this was our selected agent, clear selection
            if self.selected_agent.as_deref() == Some(aid) {
                self.selected_agent = None;
            }
        }
    }

    /// Get the agent ID at a specific index (helper for on_agent_deleted).
    fn get_agent_id_at_index(&self, index: usize) -> Option<String> {
        self.hub_handle
            .get_agents()
            .get(index)
            .map(|info| info.id.clone())
    }

    /// Handle Hub shutdown event.
    ///
    /// The Hub is shutting down. Clean up all state.
    pub fn on_hub_shutdown(&mut self) {
        self.disconnect_from_pty();
        self.selected_agent = None;
        self.hub_event_rx = None;
    }

    /// Send input to the connected PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if not connected to any PTY or if the channel is closed.
    pub fn send_input(&self, data: &[u8]) -> Result<(), String> {
        match &self.current_pty_handle {
            Some(handle) => handle.write_input_blocking(data),
            None => Err("Not connected to any PTY".to_string()),
        }
    }

    /// Send input to the connected PTY (async version).
    ///
    /// # Errors
    ///
    /// Returns an error if not connected to any PTY or if the channel is closed.
    pub async fn send_input_async(&self, data: &[u8]) -> Result<(), String> {
        match &self.current_pty_handle {
            Some(handle) => handle.write_input(data).await,
            None => Err("Not connected to any PTY".to_string()),
        }
    }

    /// Update local dimensions.
    ///
    /// Also resizes the vt100 parser and notifies the PTY if we're the size owner.
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);

        // Update parser
        if let Ok(mut parser) = self.vt100_parser.lock() {
            parser.screen_mut().set_size(rows, cols);
        }

        // Notify PTY if connected and we own the size
        if self.is_size_owner {
            if let Some(handle) = &self.current_pty_handle {
                let _ = handle.resize_blocking(self.id.clone(), rows, cols);
            }
        }
    }

    /// Check if connected (always true for TUI).
    #[must_use]
    pub fn is_connected(&self) -> bool {
        true
    }

    // === UI State methods (TuiClient's own) ===

    /// Get the currently selected agent.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&str> {
        self.selected_agent.as_deref()
    }

    /// Set the selected agent.
    pub fn set_selected_agent(&mut self, agent_id: Option<&str>) {
        self.selected_agent = agent_id.map(String::from);
    }

    /// Get the active PTY view (CLI or Server).
    #[must_use]
    pub fn active_pty_view(&self) -> PtyView {
        self.active_pty_view
    }

    /// Set the active PTY view.
    pub fn set_active_pty_view(&mut self, view: PtyView) {
        self.active_pty_view = view;
    }

    /// Toggle between CLI and Server PTY views.
    pub fn toggle_pty_view(&mut self) {
        self.active_pty_view = match self.active_pty_view {
            PtyView::Cli => PtyView::Server,
            PtyView::Server => PtyView::Cli,
        };
    }

    /// Get the connected agent (which agent's PTY we're subscribed to).
    #[must_use]
    pub fn connected_agent(&self) -> Option<&str> {
        self.connected_agent.as_deref()
    }

    // === PTY Subscription methods ===

    /// Connect to an agent's PTY via handle.
    ///
    /// Subscribes to PTY events and stores the handle for input/resize.
    pub fn connect_to_pty(&mut self, agent_id: &str, pty_handle: PtyHandle) {
        // Disconnect from previous if any
        self.disconnect_from_pty();

        // Subscribe to new PTY
        self.pty_event_rx = Some(pty_handle.subscribe());
        self.current_pty_handle = Some(pty_handle);
        self.connected_agent = Some(agent_id.to_string());

        // Notify PTY that we connected
        if let Some(handle) = &self.current_pty_handle {
            let _ = handle.connect_blocking(self.id.clone(), self.dims);
        }
    }

    /// Disconnect from the current PTY.
    pub fn disconnect_from_pty(&mut self) {
        // Notify PTY that we're disconnecting
        if let Some(handle) = &self.current_pty_handle {
            let _ = handle.disconnect_blocking(self.id.clone());
        }

        self.pty_event_rx = None;
        self.current_pty_handle = None;
        self.connected_agent = None;
    }

    /// Poll for PTY events (non-blocking).
    ///
    /// Returns the next PTY event if available, or `None` if no events pending.
    /// Returns `Err` if the channel is closed (agent terminated).
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

    /// Poll for Hub events (non-blocking).
    ///
    /// Returns the next Hub event if available, or `None` if no events pending.
    /// Returns `Err` if the channel is closed (Hub shut down).
    pub fn poll_hub_events(&mut self) -> Result<Option<HubEvent>, broadcast::error::RecvError> {
        match &mut self.hub_event_rx {
            Some(rx) => match rx.try_recv() {
                Ok(event) => Ok(Some(event)),
                Err(broadcast::error::TryRecvError::Empty) => Ok(None),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    warn!("TUI lagged behind Hub by {} events", n);
                    Ok(None)
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    Err(broadcast::error::RecvError::Closed)
                }
            },
            None => Ok(None),
        }
    }

    // === Channel setup methods ===

    /// Set the Hub event receiver.
    ///
    /// Called during initialization to receive Hub events.
    pub fn set_hub_event_rx(&mut self, rx: broadcast::Receiver<HubEvent>) {
        self.hub_event_rx = Some(rx);
    }

    /// Get a reference to the Hub handle.
    #[must_use]
    pub fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    // === Data Access Methods (Client trait) ===

    /// Get snapshot of all agents.
    ///
    /// Returns `AgentInfo` for all active agents in display order.
    /// This is a snapshot - changes won't be reflected until next call.
    #[must_use]
    pub fn get_agents(&self) -> Vec<AgentInfo> {
        self.hub_handle.get_agents()
    }

    /// Get handle for agent at index.
    ///
    /// Returns `AgentHandle` for the agent at the given index in display order,
    /// or `None` if index is out of bounds.
    #[must_use]
    pub fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        self.hub_handle.get_agent(index)
    }

    // === Accessor methods ===

    /// Get the vt100 parser for rendering.
    #[must_use]
    pub fn vt100_parser(&self) -> &Arc<Mutex<vt100::Parser>> {
        &self.vt100_parser
    }

    /// Check if this client is the size owner.
    #[must_use]
    pub fn is_size_owner(&self) -> bool {
        self.is_size_owner
    }

    /// Check if connected to a PTY.
    #[must_use]
    pub fn is_pty_connected(&self) -> bool {
        self.connected_agent.is_some()
    }
}

impl std::fmt::Debug for TuiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiClient")
            .field("id", &self.id)
            .field("dims", &self.dims)
            .field("is_size_owner", &self.is_size_owner)
            .field("selected_agent", &self.selected_agent)
            .field("active_pty_view", &self.active_pty_view)
            .field("connected_agent", &self.connected_agent)
            .field("has_pty_event_rx", &self.pty_event_rx.is_some())
            .field("has_hub_event_rx", &self.hub_event_rx.is_some())
            .field("has_current_pty_handle", &self.current_pty_handle.is_some())
            .finish()
    }
}

impl super::Client for TuiClient {
    // ============================================================
    // Identity
    // ============================================================

    fn id(&self) -> &ClientId {
        &self.id
    }

    fn dims(&self) -> (u16, u16) {
        self.dims
    }

    // ============================================================
    // Data Access (reads from Hub state)
    // ============================================================

    fn get_agents(&self) -> Vec<AgentInfo> {
        TuiClient::get_agents(self)
    }

    fn get_agent(&self, index: usize) -> Option<AgentHandle> {
        TuiClient::get_agent(self, index)
    }

    // ============================================================
    // Hub Commands (fire-and-forget via channel)
    // ============================================================

    fn request_create_agent(&self, request: CreateAgentRequest) -> Result<(), String> {
        // Convert client-facing request to Hub command request
        let hub_request = HubCreateAgentRequest::new(&request.issue_or_branch);
        let hub_request = if let Some(ref prompt) = request.prompt {
            hub_request.with_prompt(prompt)
        } else {
            hub_request
        };
        let hub_request = if let Some(ref path) = request.from_worktree {
            hub_request.from_worktree(path.clone())
        } else {
            hub_request
        };

        self.hub_handle.create_agent(hub_request)
    }

    fn request_delete_agent(&self, agent_id: &str) -> Result<(), String> {
        self.hub_handle.delete_agent(agent_id)
    }

    // ============================================================
    // Event Handlers (Hub/PTY push to Client)
    // ============================================================

    fn on_output(&mut self, data: &[u8]) {
        TuiClient::on_output(self, data);
    }

    fn on_resized(&mut self, rows: u16, cols: u16) {
        TuiClient::on_resized(self, rows, cols);
    }

    fn on_process_exit(&mut self, exit_code: Option<i32>) {
        TuiClient::on_process_exit(self, exit_code);
    }

    fn on_agent_created(&mut self, index: usize, info: &AgentInfo) {
        TuiClient::on_agent_created(self, index, info);
    }

    fn on_agent_deleted(&mut self, index: usize) {
        TuiClient::on_agent_deleted(self, index);
    }

    fn on_hub_shutdown(&mut self) {
        TuiClient::on_hub_shutdown(self);
    }

    // ============================================================
    // Connection State
    // ============================================================

    fn is_connected(&self) -> bool {
        true // TUI is always connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Helper to create a TuiClient with a mock HubHandle for testing.
    fn test_client() -> TuiClient {
        TuiClient::new(HubHandle::mock())
    }

    /// Helper to create a TuiClient with specific dimensions.
    fn test_client_with_dims(cols: u16, rows: u16) -> TuiClient {
        TuiClient::with_dims(HubHandle::mock(), cols, rows)
    }

    #[test]
    fn test_tui_client_default() {
        let client = test_client();
        assert_eq!(client.id(), &ClientId::Tui);
        assert_eq!(client.dims(), (80, 24));
        assert!(client.selected_agent().is_none());
        assert!(client.connected_agent().is_none());
        assert!(client.is_connected());
    }

    #[test]
    fn test_tui_client_with_dims() {
        let client = test_client_with_dims(120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_tui_client_selected_agent() {
        let mut client = test_client();

        client.set_selected_agent(Some("agent-123"));
        assert_eq!(client.selected_agent(), Some("agent-123"));

        client.set_selected_agent(None);
        assert!(client.selected_agent().is_none());
    }

    #[test]
    fn test_tui_client_pty_view() {
        let mut client = test_client();

        assert_eq!(client.active_pty_view(), PtyView::Cli);

        client.set_active_pty_view(PtyView::Server);
        assert_eq!(client.active_pty_view(), PtyView::Server);

        client.toggle_pty_view();
        assert_eq!(client.active_pty_view(), PtyView::Cli);

        client.toggle_pty_view();
        assert_eq!(client.active_pty_view(), PtyView::Server);
    }

    #[test]
    fn test_tui_client_update_dims() {
        let mut client = test_client();

        client.update_dims(100, 30);
        assert_eq!(client.dims(), (100, 30));

        // Verify parser was resized
        let parser = client.vt100_parser().lock().unwrap();
        assert_eq!(parser.screen().size(), (30, 100));
    }

    #[test]
    fn test_tui_client_on_output() {
        let mut client = test_client();

        client.on_output(b"Hello, World!");

        let parser = client.vt100_parser().lock().unwrap();
        let contents = parser.screen().contents();
        assert!(contents.contains("Hello, World!"));
    }

    #[test]
    fn test_tui_client_on_resized() {
        let mut client = test_client();

        client.on_resized(50, 150);
        assert_eq!(client.dims(), (150, 50));

        let parser = client.vt100_parser().lock().unwrap();
        assert_eq!(parser.screen().size(), (50, 150));
    }

    #[test]
    fn test_tui_client_on_owner_changed() {
        let mut client = test_client();
        assert!(client.is_size_owner());

        client.on_owner_changed(Some(ClientId::Browser("other".to_string())));
        assert!(!client.is_size_owner());

        client.on_owner_changed(Some(ClientId::Tui));
        assert!(client.is_size_owner());

        client.on_owner_changed(None);
        assert!(!client.is_size_owner());
    }

    #[test]
    fn test_tui_client_on_agent_deleted_with_mock_handle() {
        // With mock hub_handle, on_agent_deleted returns empty list for get_agents
        // so it won't find the agent_id at index, and selection won't be cleared
        let mut client = test_client();
        client.set_selected_agent(Some("agent-123"));

        // Delete at index 0 - mock handle returns empty agent list
        // so selected_agent won't be cleared
        client.on_agent_deleted(0);
        // Selection unchanged because we can't map index to agent_id with mock
        assert_eq!(client.selected_agent(), Some("agent-123"));
    }

    #[test]
    fn test_tui_client_connect_disconnect_pty() {
        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx, cmd_tx);

        let mut client = test_client();
        assert!(!client.is_pty_connected());

        client.connect_to_pty("agent-123", pty_handle);
        assert!(client.is_pty_connected());
        assert_eq!(client.connected_agent(), Some("agent-123"));

        client.disconnect_from_pty();
        assert!(!client.is_pty_connected());
        assert!(client.connected_agent().is_none());
    }

    #[test]
    fn test_tui_client_send_input_without_connection() {
        let client = test_client();
        let result = client.send_input(b"test");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Not connected to any PTY");
    }

    #[test]
    fn test_tui_client_poll_pty_events_without_connection() {
        let mut client = test_client();
        let result = client.poll_pty_events();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_tui_client_poll_hub_events_without_subscription() {
        let mut client = test_client();
        let result = client.poll_hub_events();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_tui_client_with_shared_parser() {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(30, 100, 5000)));
        let client = TuiClient::with_parser(HubHandle::mock(), parser.clone(), 100, 30);

        // Verify same parser is shared
        assert!(Arc::ptr_eq(&client.vt100_parser, &parser));

        // Output through client affects shared parser
        let mut client = client;
        client.on_output(b"shared");

        let p = parser.lock().unwrap();
        assert!(p.screen().contents().contains("shared"));
    }

    #[test]
    fn test_tui_client_poll_pty_events_receives_output() {
        // Set up channel and manually assign receiver to avoid calling connect_to_pty
        // which uses blocking_send (incompatible with async context).
        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);

        let mut client = test_client();
        // Directly set the receiver instead of calling connect_to_pty
        client.pty_event_rx = Some(event_tx.subscribe());

        // Send an event
        event_tx.send(PtyEvent::output(b"hello".to_vec())).unwrap();

        // Poll should receive it (uses try_recv, no async needed)
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
    fn test_tui_client_on_hub_shutdown() {
        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _cmd_rx) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx, cmd_tx);

        let mut client = test_client();
        client.set_selected_agent(Some("agent-123"));
        client.connect_to_pty("agent-123", pty_handle);

        client.on_hub_shutdown();

        assert!(client.selected_agent().is_none());
        assert!(!client.is_pty_connected());
    }

    #[test]
    fn test_tui_client_get_agents_with_mock_handle() {
        let client = test_client();
        // Mock hub_handle returns empty vec (channel is closed)
        assert!(client.get_agents().is_empty());
    }

    #[test]
    fn test_tui_client_get_agent_with_mock_handle() {
        let client = test_client();
        // Mock hub_handle returns None (channel is closed)
        assert!(client.get_agent(0).is_none());
    }

    #[test]
    fn test_tui_client_hub_handle_accessor() {
        let client = test_client();
        // hub_handle is always set (required in constructor)
        let _ = client.hub_handle();
    }
}
