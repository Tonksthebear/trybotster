# Client Abstraction Implementation Plan

## Overview

Refactor Hub architecture to support independent agent selection per client (TUI, Browser A, Browser B can each view different agents simultaneously).

**Key Principles:**
- Hub owns all data (agents, worktrees, PTY output)
- Clients own their view state (which agent, terminal dims)
- Client trait defines the interface, implementations handle transport
- TUI is just another client (no special-casing in Hub logic)

---

## Part 1: Core Types

### File: `cli/src/client/mod.rs`

```rust
//! Client abstraction for TUI and browser connections.

mod types;
mod registry;
mod tui;
mod browser;

pub use types::*;
pub use registry::ClientRegistry;
pub use tui::TuiClient;
pub use browser::BrowserClient;

use crate::agent::AgentInfo;

/// Unique identifier for a client session
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientId {
    Tui,
    Browser(String), // Signal identity key
}

impl ClientId {
    pub fn browser(identity: impl Into<String>) -> Self {
        ClientId::Browser(identity.into())
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientId::Tui => write!(f, "tui"),
            ClientId::Browser(id) => write!(f, "browser:{}", &id[..8.min(id.len())]),
        }
    }
}

/// Per-client view state
#[derive(Debug, Clone, Default)]
pub struct ClientState {
    pub selected_agent: Option<String>,  // Session key
    pub dims: Option<(u16, u16)>,        // Terminal dimensions
    // Note: active_pty is NOT stored - always default to Cli on agent selection
    // Note: scroll_offset is NOT stored - handled locally by xterm.js / vt100_parser
}

/// The Client trait - interface for all client types
pub trait Client: Send {
    /// Unique identifier for this client
    fn id(&self) -> &ClientId;

    /// Access view state
    fn state(&self) -> &ClientState;
    fn state_mut(&mut self) -> &mut ClientState;

    // === Receive: Hub pushes data to client ===

    /// Terminal output from PTY (raw bytes)
    fn receive_output(&mut self, data: &[u8]);

    /// Scrollback history (on agent selection)
    fn receive_scrollback(&mut self, lines: Vec<String>);

    /// Full agent list (on change or request)
    fn receive_agent_list(&mut self, agents: Vec<AgentInfo>);

    /// Available worktrees (on request)
    fn receive_worktree_list(&mut self, worktrees: Vec<WorktreeInfo>);

    /// Response to client action (confirmation or error)
    fn receive_response(&mut self, response: Response);

    // === State mutations: Hub calls these after processing actions ===

    /// Update selection (called by Hub after validation)
    fn select_agent(&mut self, agent_key: &str) {
        self.state_mut().selected_agent = Some(agent_key.to_string());
    }

    /// Clear selection (agent was deleted)
    fn clear_selection(&mut self) {
        self.state_mut().selected_agent = None;
    }

    /// Update dimensions
    fn resize(&mut self, cols: u16, rows: u16) {
        self.state_mut().dims = Some((cols, rows));
    }

    // === Lifecycle ===

    /// Called periodically to flush buffered output (for batching)
    fn flush(&mut self) {}

    /// Check if client connection is healthy
    fn is_connected(&self) -> bool { true }
}
```

### File: `cli/src/client/types.rs`

```rust
//! Types for client communication.

use serde::{Deserialize, Serialize};

/// Response to a client action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    AgentSelected { id: String },
    AgentCreated { id: String },
    AgentDeleted { id: String },
    Error { message: String },
}

/// Request to create an agent
#[derive(Debug, Clone)]
pub struct CreateAgentRequest {
    pub issue_or_branch: String,
    pub prompt: Option<String>,
    pub from_worktree: Option<std::path::PathBuf>,
}

/// Request to delete an agent
#[derive(Debug, Clone)]
pub struct DeleteAgentRequest {
    pub agent_key: String,
    pub delete_worktree: bool,
}

/// Worktree info for client display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
}
```

---

## Part 2: Client Registry with Reverse Index

### File: `cli/src/client/registry.rs`

```rust
//! Client registry with optimized viewer tracking.

use std::collections::{HashMap, HashSet};
use super::{Client, ClientId};

/// Registry of all connected clients with reverse index for PTY routing
pub struct ClientRegistry {
    /// All clients by ID
    clients: HashMap<ClientId, Box<dyn Client>>,

    /// Reverse index: agent_key -> set of client IDs viewing that agent
    /// Enables O(1) lookup for PTY output routing
    viewers: HashMap<String, HashSet<ClientId>>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            viewers: HashMap::new(),
        }
    }

    /// Register a new client
    pub fn register(&mut self, client: Box<dyn Client>) {
        let id = client.id().clone();
        self.clients.insert(id, client);
    }

    /// Unregister a client, cleaning up viewer index
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

    /// Get client by ID
    pub fn get(&self, id: &ClientId) -> Option<&dyn Client> {
        self.clients.get(id).map(|c| c.as_ref())
    }

    /// Get mutable client by ID
    pub fn get_mut(&mut self, id: &ClientId) -> Option<&mut Box<dyn Client>> {
        self.clients.get_mut(id)
    }

    /// Update viewer index when client changes selection
    pub fn update_selection(&mut self, client_id: &ClientId, old_agent: Option<&str>, new_agent: Option<&str>) {
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

    /// Get all client IDs viewing a specific agent (O(1) lookup)
    pub fn viewers_of(&self, agent_key: &str) -> impl Iterator<Item = &ClientId> {
        self.viewers
            .get(agent_key)
            .into_iter()
            .flat_map(|set| set.iter())
    }

    /// Iterate all clients
    pub fn iter(&self) -> impl Iterator<Item = (&ClientId, &Box<dyn Client>)> {
        self.clients.iter()
    }

    /// Iterate all clients mutably
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ClientId, &mut Box<dyn Client>)> {
        self.clients.iter_mut()
    }

    /// Get all client IDs
    pub fn client_ids(&self) -> impl Iterator<Item = &ClientId> {
        self.clients.keys()
    }

    /// Number of connected clients
    pub fn len(&self) -> usize {
        self.clients.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Remove agent from all viewer indices (when agent is deleted)
    pub fn remove_agent_viewers(&mut self, agent_key: &str) {
        self.viewers.remove(agent_key);
    }

    /// Flush all clients (for batched output)
    pub fn flush_all(&mut self) {
        for client in self.clients.values_mut() {
            client.flush();
        }
    }
}

impl Default for ClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}
```

---

## Part 3: TUI Client Implementation

### File: `cli/src/client/tui.rs`

```rust
//! TUI client implementation.
//!
//! TUI reads directly from Hub state during render cycle,
//! so most receive methods are no-ops.

use super::{Client, ClientId, ClientState, Response, WorktreeInfo};
use crate::agent::AgentInfo;

pub struct TuiClient {
    id: ClientId,
    state: ClientState,
}

impl TuiClient {
    pub fn new() -> Self {
        Self {
            id: ClientId::Tui,
            state: ClientState::default(),
        }
    }
}

impl Default for TuiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Client for TuiClient {
    fn id(&self) -> &ClientId {
        &self.id
    }

    fn state(&self) -> &ClientState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut ClientState {
        &mut self.state
    }

    fn receive_output(&mut self, _data: &[u8]) {
        // No-op: TUI reads from agent.cli_pty.vt100_parser during render
    }

    fn receive_scrollback(&mut self, _lines: Vec<String>) {
        // No-op: TUI reads from agent's buffer directly
    }

    fn receive_agent_list(&mut self, _agents: Vec<AgentInfo>) {
        // No-op: TUI iterates hub.state.agents during render
    }

    fn receive_worktree_list(&mut self, _worktrees: Vec<WorktreeInfo>) {
        // No-op: TUI reads hub.state.available_worktrees during render
    }

    fn receive_response(&mut self, _response: Response) {
        // Could show toast/notification, but TUI re-renders constantly anyway
    }
}
```

---

## Part 4: Browser Client Implementation

### File: `cli/src/client/browser.rs`

```rust
//! Browser client implementation with WebSocket transport.

use std::time::{Duration, Instant};
use super::{Client, ClientId, ClientState, Response, WorktreeInfo};
use crate::agent::AgentInfo;
use crate::relay::{TerminalMessage, TerminalOutputSender};

/// Output batching interval (~60fps)
const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// Browser connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
}

pub struct BrowserClient {
    id: ClientId,
    state: ClientState,

    // Transport
    identity: String,  // Signal identity key (for encryption routing)
    sender: TerminalOutputSender,

    // Connection tracking
    connection: ConnectionState,

    // Output batching
    output_buffer: Vec<u8>,
    last_flush: Instant,
}

impl BrowserClient {
    pub fn new(identity: String, sender: TerminalOutputSender) -> Self {
        Self {
            id: ClientId::Browser(identity.clone()),
            state: ClientState::default(),
            identity,
            sender,
            connection: ConnectionState::Connected,
            output_buffer: Vec::with_capacity(4096),
            last_flush: Instant::now(),
        }
    }

    /// Get the Signal identity key
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Mark as disconnected
    pub fn set_disconnected(&mut self) {
        self.connection = ConnectionState::Disconnected;
    }

    /// Send a terminal message over the encrypted channel
    fn send_message(&self, msg: TerminalMessage) {
        if self.connection == ConnectionState::Connected {
            // sender.send_to handles encryption and WebSocket delivery
            if let Err(e) = self.sender.try_send_to(&self.identity, msg) {
                log::warn!("Failed to send to browser {}: {}", &self.identity[..8], e);
            }
        }
    }

    /// Flush buffered output
    fn flush_output(&mut self) {
        if !self.output_buffer.is_empty() {
            let data = String::from_utf8_lossy(&self.output_buffer).to_string();
            self.send_message(TerminalMessage::Output { data });
            self.output_buffer.clear();
            self.last_flush = Instant::now();
        }
    }
}

impl Client for BrowserClient {
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
        // Buffer output for batching
        self.output_buffer.extend_from_slice(data);

        // Flush if interval elapsed (prevents flooding)
        if self.last_flush.elapsed() >= FLUSH_INTERVAL {
            self.flush_output();
        }
    }

    fn receive_scrollback(&mut self, lines: Vec<String>) {
        self.send_message(TerminalMessage::Scrollback { lines });
    }

    fn receive_agent_list(&mut self, agents: Vec<AgentInfo>) {
        self.send_message(TerminalMessage::Agents { agents });
    }

    fn receive_worktree_list(&mut self, worktrees: Vec<WorktreeInfo>) {
        self.send_message(TerminalMessage::Worktrees {
            worktrees: worktrees.into_iter().map(|w| crate::relay::types::WorktreeInfo {
                path: w.path,
                branch: w.branch,
            }).collect()
        });
    }

    fn receive_response(&mut self, response: Response) {
        let msg = match response {
            Response::AgentSelected { id } => TerminalMessage::AgentSelected { id },
            Response::AgentCreated { id } => TerminalMessage::AgentCreated { id },
            Response::AgentDeleted { id } => TerminalMessage::AgentDeleted { id },
            Response::Error { message } => TerminalMessage::Error { message },
        };
        self.send_message(msg);
    }

    fn flush(&mut self) {
        self.flush_output();
    }

    fn is_connected(&self) -> bool {
        self.connection == ConnectionState::Connected
    }
}
```

---

## Part 5: Hub Changes

### File: `cli/src/hub/mod.rs` (modifications)

```rust
// Replace browser: BrowserState with:
pub clients: ClientRegistry,

// Remove from HubState:
// - selected: usize (TUI client owns its selection now)

// Keep in Hub:
// - terminal_dims for TUI (or move to TuiClient)
```

### Hub Initialization

```rust
impl Hub {
    pub fn new(/* ... */) -> Self {
        let mut hub = Self {
            // ...
            clients: ClientRegistry::new(),
            // ...
        };

        // Register TUI as a client
        hub.clients.register(Box::new(TuiClient::new()));

        hub
    }
}
```

### Hub Dispatch Changes

```rust
impl Hub {
    pub fn dispatch(&mut self, action: HubAction) {
        match action {
            HubAction::SelectAgent { client_id, agent_key } => {
                self.handle_select_agent(client_id, agent_key);
            }

            HubAction::SendInput { client_id, data } => {
                self.handle_send_input(client_id, data);
            }

            HubAction::Resize { client_id, cols, rows } => {
                self.handle_resize(client_id, cols, rows);
            }

            HubAction::CreateAgent { client_id, request } => {
                self.handle_create_agent(client_id, request);
            }

            HubAction::DeleteAgent { client_id, request } => {
                self.handle_delete_agent(client_id, request);
            }

            HubAction::RequestAgentList { client_id } => {
                self.send_agent_list_to(&client_id);
            }

            HubAction::RequestWorktreeList { client_id } => {
                self.send_worktree_list_to(&client_id);
            }

            HubAction::ClientConnected { client_id } => {
                self.handle_client_connected(client_id);
            }

            HubAction::ClientDisconnected { client_id } => {
                self.handle_client_disconnected(client_id);
            }

            // ... other actions
        }
    }

    fn handle_select_agent(&mut self, client_id: ClientId, agent_key: String) {
        // Validate agent exists
        if !self.state.agents.contains_key(&agent_key) {
            self.send_error_to(&client_id, "Agent not found");
            return;
        }

        // Get old selection for viewer index update
        let old_selection = self.clients.get(&client_id)
            .and_then(|c| c.state().selected_agent.clone());

        // Update viewer index
        self.clients.update_selection(
            &client_id,
            old_selection.as_deref(),
            Some(&agent_key)
        );

        // Update client state and send data
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.select_agent(&agent_key);

            // Send scrollback from agent's CLI PTY (default view)
            if let Some(agent) = self.state.agents.get(&agent_key) {
                let scrollback = agent.cli_pty.get_scrollback_lines();
                client.receive_scrollback(scrollback);
            }

            client.receive_response(Response::AgentSelected { id: agent_key });
        }
    }

    fn handle_send_input(&mut self, client_id: ClientId, data: Vec<u8>) {
        // Get client's selected agent
        let agent_key = match self.clients.get(&client_id) {
            Some(client) => client.state().selected_agent.clone(),
            None => return,
        };

        // Route input to agent's CLI PTY
        if let Some(key) = agent_key {
            if let Some(agent) = self.state.agents.get_mut(&key) {
                agent.cli_pty.write_input(&data);
            }
        }
    }

    fn handle_resize(&mut self, client_id: ClientId, cols: u16, rows: u16) {
        // Update client dims
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.resize(cols, rows);
        }

        // Resize the PTY if client is viewing an agent
        let agent_key = self.clients.get(&client_id)
            .and_then(|c| c.state().selected_agent.clone());

        if let Some(key) = agent_key {
            if let Some(agent) = self.state.agents.get_mut(&key) {
                agent.resize(cols, rows);
            }
        }
    }

    fn handle_create_agent(&mut self, client_id: ClientId, request: CreateAgentRequest) {
        match self.create_agent_internal(request) {
            Ok(agent_key) => {
                // Notify requesting client
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.receive_response(Response::AgentCreated { id: agent_key.clone() });
                }
                // Broadcast updated list to all clients
                self.broadcast_agent_list();
            }
            Err(e) => {
                self.send_error_to(&client_id, &e.to_string());
            }
        }
    }

    fn handle_delete_agent(&mut self, client_id: ClientId, request: DeleteAgentRequest) {
        // Clear selection for any client viewing this agent
        let viewers: Vec<ClientId> = self.clients
            .viewers_of(&request.agent_key)
            .cloned()
            .collect();

        for viewer_id in &viewers {
            if let Some(client) = self.clients.get_mut(viewer_id) {
                client.clear_selection();
            }
        }

        // Remove from viewer index
        self.clients.remove_agent_viewers(&request.agent_key);

        // Delete the agent
        match self.delete_agent_internal(&request.agent_key, request.delete_worktree) {
            Ok(()) => {
                if let Some(client) = self.clients.get_mut(&client_id) {
                    client.receive_response(Response::AgentDeleted { id: request.agent_key });
                }
                self.broadcast_agent_list();
            }
            Err(e) => {
                self.send_error_to(&client_id, &e.to_string());
            }
        }
    }

    fn handle_client_connected(&mut self, client_id: ClientId) {
        // For browser clients, send initial agent list
        if let ClientId::Browser(_) = &client_id {
            self.send_agent_list_to(&client_id);
        }
    }

    fn handle_client_disconnected(&mut self, client_id: ClientId) {
        self.clients.unregister(&client_id);
        log::info!("Client disconnected: {}", client_id);
    }

    // === Helper methods ===

    fn send_agent_list_to(&mut self, client_id: &ClientId) {
        let agents: Vec<AgentInfo> = self.state.agents
            .iter()
            .map(|(key, agent)| agent.to_info(key))
            .collect();

        if let Some(client) = self.clients.get_mut(client_id) {
            client.receive_agent_list(agents);
        }
    }

    fn send_worktree_list_to(&mut self, client_id: &ClientId) {
        let worktrees: Vec<WorktreeInfo> = self.state.available_worktrees
            .iter()
            .map(|(path, branch)| WorktreeInfo {
                path: path.clone(),
                branch: branch.clone(),
            })
            .collect();

        if let Some(client) = self.clients.get_mut(client_id) {
            client.receive_worktree_list(worktrees);
        }
    }

    fn send_error_to(&mut self, client_id: &ClientId, message: &str) {
        if let Some(client) = self.clients.get_mut(client_id) {
            client.receive_response(Response::Error { message: message.to_string() });
        }
    }

    fn broadcast_agent_list(&mut self) {
        let agents: Vec<AgentInfo> = self.state.agents
            .iter()
            .map(|(key, agent)| agent.to_info(key))
            .collect();

        for client in self.clients.iter_mut().map(|(_, c)| c) {
            client.receive_agent_list(agents.clone());
        }
    }

    /// Route PTY output to all clients viewing that agent
    pub fn broadcast_pty_output(&mut self, agent_key: &str, data: &[u8]) {
        let viewer_ids: Vec<ClientId> = self.clients
            .viewers_of(agent_key)
            .cloned()
            .collect();

        for client_id in viewer_ids {
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.receive_output(data);
            }
        }
    }
}
```

---

## Part 6: HubAction Changes

### File: `cli/src/hub/actions.rs`

```rust
pub enum HubAction {
    // === Client-scoped actions ===

    SelectAgent { client_id: ClientId, agent_key: String },
    SendInput { client_id: ClientId, data: Vec<u8> },
    Resize { client_id: ClientId, cols: u16, rows: u16 },

    CreateAgent { client_id: ClientId, request: CreateAgentRequest },
    DeleteAgent { client_id: ClientId, request: DeleteAgentRequest },

    RequestAgentList { client_id: ClientId },
    RequestWorktreeList { client_id: ClientId },

    // === Client lifecycle ===

    ClientConnected { client_id: ClientId },
    ClientDisconnected { client_id: ClientId },

    // === Global actions (no client context) ===

    Quit,
    TogglePolling,
    RefreshWorktrees,

    // TUI-specific (uses ClientId::Tui implicitly)
    OpenMenu,
    CloseModal,
    ShowConnectionCode,
    CopyConnectionUrl,

    None,
}
```

---

## Part 7: Event Loop Changes

### File: `cli/src/hub/run.rs`

```rust
// In the main event loop:

// 1. Poll PTY output from all agents
for (agent_key, agent) in &mut hub.state.agents {
    if let Some(output) = agent.cli_pty.drain_raw_output() {
        if !output.is_empty() {
            hub.broadcast_pty_output(&agent_key, &output);
        }
    }

    if let Some(server_pty) = &mut agent.server_pty {
        if let Some(output) = server_pty.drain_raw_output() {
            // Server PTY output - for now, only route if client is viewing server
            // (Future: track per-client PTY view preference)
        }
    }
}

// 2. Flush all clients (batched output)
hub.clients.flush_all();

// 3. Process browser events
while let Ok(event) = hub.browser_event_rx.try_recv() {
    let action = browser_event_to_action(event, &browser_identity);
    hub.dispatch(action);
}

// 4. TUI rendering reads directly from Hub state
// No changes needed - TUI queries hub.clients.get(&ClientId::Tui) for its selection
```

---

## Part 8: TUI Input Changes

### File: `cli/src/tui/input.rs`

```rust
pub fn event_to_hub_action(event: Event, hub: &Hub) -> HubAction {
    let client_id = ClientId::Tui;

    match event {
        Event::Key(KeyEvent { code: KeyCode::Tab, .. }) => {
            // Select next agent (TUI-specific navigation)
            let next_key = hub.get_next_agent_key(&client_id);
            if let Some(key) = next_key {
                HubAction::SelectAgent { client_id, agent_key: key }
            } else {
                HubAction::None
            }
        }

        Event::Key(KeyEvent { code: KeyCode::Char(c), .. }) if hub.mode == AppMode::Normal => {
            HubAction::SendInput {
                client_id,
                data: vec![c as u8]
            }
        }

        Event::Resize(cols, rows) => {
            HubAction::Resize { client_id, cols, rows }
        }

        // ... etc
    }
}
```

---

## Part 9: Browser Event Changes

### File: `cli/src/relay/events.rs`

```rust
pub fn browser_event_to_action(event: BrowserEvent, browser_identity: &str) -> HubAction {
    let client_id = ClientId::browser(browser_identity);

    match event {
        BrowserEvent::SelectAgent { id } => {
            HubAction::SelectAgent { client_id, agent_key: id }
        }

        BrowserEvent::Input(data) => {
            HubAction::SendInput { client_id, data: data.into_bytes() }
        }

        BrowserEvent::Resize { cols, rows } => {
            HubAction::Resize { client_id, cols, rows }
        }

        BrowserEvent::CreateAgent { issue_or_branch, prompt } => {
            HubAction::CreateAgent {
                client_id,
                request: CreateAgentRequest {
                    issue_or_branch,
                    prompt,
                    from_worktree: None,
                },
            }
        }

        BrowserEvent::DeleteAgent { id, delete_worktree } => {
            HubAction::DeleteAgent {
                client_id,
                request: DeleteAgentRequest {
                    agent_key: id,
                    delete_worktree: delete_worktree.unwrap_or(false),
                },
            }
        }

        BrowserEvent::ListAgents => {
            HubAction::RequestAgentList { client_id }
        }

        BrowserEvent::ListWorktrees => {
            HubAction::RequestWorktreeList { client_id }
        }

        BrowserEvent::Connected { public_key, device_name } => {
            // Register browser client in Hub
            HubAction::ClientConnected { client_id }
        }

        BrowserEvent::Disconnected => {
            HubAction::ClientDisconnected { client_id }
        }

        // ... etc
    }
}
```

---

## Part 10: TUI Render Changes

### File: `cli/src/tui/render.rs`

```rust
pub fn render(f: &mut Frame, hub: &Hub) {
    // Get TUI client's selected agent
    let tui_selection = hub.clients.get(&ClientId::Tui)
        .and_then(|c| c.state().selected_agent.as_ref());

    let selected_agent = tui_selection
        .and_then(|key| hub.state.agents.get(key));

    // Render agent pane with selected agent
    render_agent_pane(f, selected_agent, area);

    // Render agent list with current selection highlighted
    render_agent_list(f, &hub.state.agents, tui_selection, list_area);
}
```

---

## Migration Checklist

### Phase 1: Core Types (no behavior change)
- [ ] Create `cli/src/client/mod.rs` with trait and ClientId
- [ ] Create `cli/src/client/types.rs` with Response, CreateAgentRequest, etc.
- [ ] Create `cli/src/client/registry.rs` with ClientRegistry
- [ ] Create `cli/src/client/tui.rs` with TuiClient
- [ ] Create `cli/src/client/browser.rs` with BrowserClient (stub)
- [ ] Add `pub mod client;` to `cli/src/lib.rs`
- [ ] Verify compiles

### Phase 2: Hub Integration
- [ ] Add `clients: ClientRegistry` to Hub struct
- [ ] Register TuiClient in Hub::new()
- [ ] Update HubAction enum with client_id variants
- [ ] Implement dispatch handlers for new actions
- [ ] Add helper methods (send_agent_list_to, broadcast_pty_output, etc.)

### Phase 3: TUI Migration
- [ ] Update tui/input.rs to use ClientId::Tui in actions
- [ ] Update tui/render.rs to get selection from TuiClient
- [ ] Remove `selected` from HubState (TUI owns its selection)
- [ ] Verify TUI works with single client

### Phase 4: Browser Migration
- [ ] Update relay/events.rs to use browser_event_to_action
- [ ] Create BrowserClient when browser connects
- [ ] Register BrowserClient in ClientRegistry
- [ ] Update relay/connection.rs to route through client
- [ ] Remove BrowserState (replaced by BrowserClient instances)

### Phase 5: PTY Output Routing
- [ ] Update event loop to call broadcast_pty_output
- [ ] Implement output batching in BrowserClient
- [ ] Add flush_all call in event loop
- [ ] Test multi-client viewing same agent

### Phase 6: Testing & Cleanup
- [ ] Test TUI agent selection
- [ ] Test browser agent selection
- [ ] Test TUI + browser viewing different agents
- [ ] Test browser A + browser B viewing different agents
- [ ] Test agent deletion clears viewer selections
- [ ] Remove dead code from old architecture
- [ ] Update unit tests

---

## Files to Create

| File | Description |
|------|-------------|
| `cli/src/client/mod.rs` | Client trait, ClientId, ClientState |
| `cli/src/client/types.rs` | Response, CreateAgentRequest, DeleteAgentRequest, WorktreeInfo |
| `cli/src/client/registry.rs` | ClientRegistry with viewer index |
| `cli/src/client/tui.rs` | TuiClient implementation |
| `cli/src/client/browser.rs` | BrowserClient implementation |

## Files to Modify

| File | Changes |
|------|---------|
| `cli/src/lib.rs` | Add `pub mod client;` |
| `cli/src/hub/mod.rs` | Add `clients: ClientRegistry`, remove browser state |
| `cli/src/hub/state.rs` | Remove `selected: usize` |
| `cli/src/hub/actions.rs` | Add client_id to action variants |
| `cli/src/hub/run.rs` | Update event loop for PTY routing, flush |
| `cli/src/tui/input.rs` | Use ClientId::Tui in actions |
| `cli/src/tui/render.rs` | Get selection from TuiClient |
| `cli/src/relay/events.rs` | Map browser events to client-scoped actions |
| `cli/src/relay/connection.rs` | Create/register BrowserClient |
| `cli/src/relay/state.rs` | Remove or gut BrowserState |

---

## Notes

- **PTY View (Cli vs Server)**: For simplicity, always default to CLI PTY when selecting agent. Server PTY viewing can be added later as per-client preference.

- **Scroll Offset**: Handled locally by xterm.js (browser) and vt100_parser (TUI). Not tracked in ClientState.

- **Agent List Updates**: Always send full list (simpler than deltas). Clients replace their local list.

- **Output Batching**: BrowserClient buffers output and flushes at ~60fps to prevent WebSocket flooding.

- **Reverse Index**: `viewers` HashMap enables O(1) lookup for PTY output routing instead of O(n) iteration.
