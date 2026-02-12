# Client Architecture Refactor Design

**Status:** Draft
**Created:** 2026-01-21
**Author:** Claude + Jason

---

## 1. Overview

This document describes a refactor of the client/PTY architecture in botster to achieve:

- **Clean separation of concerns** - PTY Sessions handle I/O, Clients handle transport
- **Event-driven pub/sub** - Components communicate via broadcast events, not direct calls
- **Uniform client interface** - TUI and Browser implement the same trait
- **Modular, testable design** - Components can be tested in isolation
- **Easy swapping** - Threads ↔ async tasks, transport mechanisms, etc.

### Current Architecture Problems

1. `active_pty` lives on Agent, shared across all clients (should be per-client)
2. Channels connect at agent spawn, not when clients connect to PTY
3. No resize triggered when switching PTY views
4. Client trait is thin - doesn't capture the full interaction model
5. Hub maintains client→agent mapping; PTY sessions don't know their viewers

---

## 2. Architecture Decisions & Rationale

### 2.1 PTY Sessions Track Connected Clients

**Decision:** `PtySession` maintains a list of connected clients with their dimensions.

**Rationale:** Clients asynchronously hop in/out of PTY sessions independently. The PTY session is the natural place to track who's connected because:
- It owns the resize logic (newest client = size owner)
- It broadcasts output to connected clients
- It needs to handle fallback when clients disconnect

**Alternative considered:** Hub maintains reverse mapping. Rejected because it centralizes logic that belongs with the PTY.

### 2.2 Event-Driven Pub/Sub for Output

**Decision:** Use `tokio::sync::broadcast` for PTY output events. PTY emits, clients subscribe.

**Rationale:**
- PTY reader thread just sends - no knowledge of clients needed
- Each client receives independently - no shared mutable state
- Natural fit for multithreading - subscribers run in their own threads/tasks
- Decouples producer from consumers

**Code pattern:**
```rust
// PTY Session
pub struct PtySession {
    output_tx: broadcast::Sender<PtyEvent>,
}

impl PtySession {
    fn emit_output(&self, data: Vec<u8>) {
        let _ = self.output_tx.send(PtyEvent::Output(data));
    }
}

// Client subscribes
let mut rx = pty_session.subscribe();
loop {
    match rx.recv().await {
        Ok(PtyEvent::Output(data)) => client.on_output(&data),
        // ...
    }
}
```

### 2.3 Newest Client Owns Dimensions

**Decision:** The most recently connected client becomes the "size owner". When they disconnect, ownership falls back to the next most recent.

**Rationale:**
- Simple, intuitive rule
- The user who just connected most likely wants to interact
- Avoids complex negotiation protocols
- Small race window is acceptable (user experience, not correctness issue)

### 2.4 Client Trait as Uniform Interface

**Decision:** Both TUI and Browser implement `Client` trait. The trait defines what clients *do* (on_output, on_resize, etc.), implementations handle *how*. All clients run in their own threads and use Hub command channels.

**Rationale:**
- Single interface for Hub/PTY to interact with any client type
- TUI and Browser differ only in transport, not semantics
- Easy to add new client types (mobile app, CLI viewer, etc.)
- Testable - can mock clients for unit tests
- Uniform testing - all clients go through same command interface
- No special-casing in Hub for different client types

**Note on multiple PTY connections:** The architecture explicitly supports a single client connecting to multiple PtySessions simultaneously (e.g., split view). Each PtySession independently tracks its connected clients. Current implementations (TUI, Browser) use one active PTY for simplicity, but that's a client-side choice.

### 2.5 TUI Owns Its Own vt100 Parser

**Decision:** Move `vt100_parser` from PtySession to TuiClient. PtySession just emits raw bytes.

**Rationale:**
- Browser doesn't need local terminal emulation (xterm.js does it)
- TUI is the only client that renders locally
- Keeps PtySession as pure I/O - no rendering concerns
- Each client type handles data in its own way

### 2.6 Browser Manages Its Own Channels

**Decision:** BrowserClient is responsible for connecting/disconnecting e2e encrypted channels when hopping between PTY sessions. All channels use reliable mode.

**Rationale:**
- TUI doesn't need channels (local access)
- Only browsers need encryption overhead
- Channel lifecycle tied to client's PTY view, not agent lifetime
- Cleaner separation - PTY doesn't know about encryption
- **Reliable mode ensures delivery** - handles backpressure, retries on failure, no dropped messages

### 2.7 Actor Pattern for Hub Commands

**Decision:** Use `mpsc` channels for client→Hub commands (create agent, delete agent, etc.).

**Rationale:**
- Many-to-one relationship: multiple clients → single Hub
- Avoids lock contention on Hub state
- Serializes command processing naturally
- Explicit, traceable command flow

**Code pattern:**
```rust
// Client sends command
hub_tx.send(HubCommand::CreateAgent { request, response_tx }).await;

// Hub processes in its loop
while let Some(cmd) = rx.recv().await {
    match cmd {
        HubCommand::CreateAgent { request, response_tx } => {
            let result = self.create_agent(request);
            let _ = response_tx.send(result);
        }
    }
}
```

### 2.8 Threads vs Async Tasks

**Decision:** Design for easy swapping. Start with threads, migrate to async tasks if needed.

**Rationale:**
- Client business logic (on_output, on_resize) is identical either way
- Only the "runner" (spawn + event loop) differs
- `tokio::sync::broadcast` supports both (`recv().await` or `blocking_recv()`)
- Threads are simpler mental model; async scales better

**Swapping pattern:**
```rust
// Thread runner
std::thread::spawn(move || {
    loop {
        let event = rx.blocking_recv();
        client.on_output(&event);
    }
});

// Async runner (swap later if needed)
tokio::spawn(async move {
    loop {
        let event = rx.recv().await;
        client.on_output(&event);
    }
});
```

---

## 3. Component Responsibilities

### 3.1 Hub

**Owns:**
- Collection of Agents
- Hub-level broadcast channel for global events
- Command receiver for client requests

**Broadcasts (HubEvent):**
- `AgentCreated { agent_id, info }`
- `AgentDeleted { agent_id }`
- `AgentStatusChanged { agent_id, status }`
- `HubShutdown`

**Receives (HubCommand):**
- `CreateAgent { request, response_tx }`
- `DeleteAgent { agent_id, response_tx }`
- `ListAgents { response_tx }`
- `GetAgentInfo { agent_id, response_tx }`

**Does NOT:**
- Track which client is viewing which PTY (that's PtySession's job)
- Route PTY output (that's pub/sub)
- Manage resize logic (that's PtySession's job)

### 3.2 Agent

**Owns:**
- Multiple PtySession instances (cli_pty at index 0, server_pty at index 1)
- Agent metadata (repo, issue_number, branch_name, etc.)
- Preview channel (for HTTP tunnel preview)

**Does NOT:**
- Track `active_pty` (that's per-client state)
- Manage client connections (that's PtySession's job)

### 3.3 PtySession

**Owns:**
- PTY I/O primitives (master_pty, writer, reader thread)
- Raw byte scrollback buffer (for history replay)
- Connected clients list with dimensions
- Output broadcast channel

**Broadcasts (PtyEvent):**
- `Output { data: Vec<u8> }`
- `ProcessExited { exit_code: Option<i32> }`
- `Resized { rows: u16, cols: u16 }`
- `OwnerChanged { client_id: Option<ClientId> }`

**Exposes:**
- `connect(client_id, dims)` → subscription receiver
- `disconnect(client_id)`
- `client_resized(client_id, dims)`
- `write_input(data)`
- `get_scrollback()` → raw bytes for replay

**Internal logic:**
- Tracks connected clients ordered by connection time
- Size owner = `connected_clients.last()`
- On connect: add to list, resize if new owner
- On disconnect: remove from list, resize to new owner's dims
- On client resize: update stored dims, resize if owner

### 3.4 Client (Trait)

```rust
/// Uniform interface for all client types.
///
/// Clients connect to PTY sessions and receive events. Each client
/// implements the trait differently based on its transport mechanism.
/// All clients run in their own thread and use the same interfaces.
pub trait Client: Send {
    // === Identity ===

    /// Unique identifier for this client.
    fn id(&self) -> &ClientId;

    /// Current terminal dimensions.
    fn dims(&self) -> (u16, u16);

    // === PTY Event Handlers ===

    /// Called when PTY emits output.
    fn on_output(&mut self, data: &[u8]);

    /// Called when PTY is resized.
    fn on_resize(&mut self, rows: u16, cols: u16);

    /// Called when PTY process exits.
    fn on_process_exit(&mut self, exit_code: Option<i32>);

    /// Called when size ownership changes.
    fn on_owner_changed(&mut self, new_owner: Option<ClientId>);

    // === Hub Event Handlers ===

    /// Called when an agent is created.
    fn on_agent_created(&mut self, agent_id: &str, info: &AgentInfo);

    /// Called when an agent is deleted.
    fn on_agent_deleted(&mut self, agent_id: &str);

    /// Called when Hub is shutting down.
    fn on_hub_shutdown(&mut self);

    // === Actions ===

    /// Send input to the PTY this client is viewing.
    fn send_input(&mut self, data: &[u8]);

    /// Update this client's dimensions.
    fn resize(&mut self, rows: u16, cols: u16);

    // === Hub Commands (via channel, but exposed uniformly) ===

    /// Request to create an agent. Returns via callback/future.
    fn create_agent(&self, request: CreateAgentRequest);

    /// Request to delete an agent.
    fn delete_agent(&self, agent_id: &str);

    /// Request agent list.
    fn list_agents(&self);
}
```

**Note:** Hub commands (create_agent, delete_agent, list_agents) are sent via the Hub command channel internally. The trait exposes them uniformly so all clients use the same interface, enabling consistent testing.
```

### 3.5 TuiClient

**Owns:**
- `vt100_parser` for local terminal emulation
- Subscription to currently viewed PtySession's broadcast
- Terminal dimensions
- Hub command sender (same as BrowserClient - uniformity)

**Runs in its own thread** - just like BrowserClient. This ensures:
- Uniform testing - all clients use the same command interface
- No special-casing in Hub
- Clean separation from Hub's main loop

**Implements Client:**
- `on_output`: Feed bytes to `vt100_parser`
- `send_input`: Write to PTY (via subscription context)
- Uses Hub command channel for agent CRUD (same as Browser)

**Render loop:**
- Runs in TUI thread
- Reads from `self.vt100_parser` to get screen state
- Renders via ratatui

### 3.6 BrowserClient

**Owns:**
- Browser identity (Signal public key)
- Terminal dimensions
- Hub channel (1) - encrypted ActionCable for commands/events
- PTY channels (0 to N) - encrypted ActionCable per connected PTY

**Structure:**
```rust
BrowserClient {
    id: ClientId::Browser(identity),
    dims: (u16, u16),
    hub_channel: ActionCableChannel,  // Always connected
    pty_channels: HashMap<PtyKey, ActionCableChannel>,  // Connect on demand
}
```

**Implements Client:**
- `on_output`: Encrypt and send via PTY channel
- `send_input`: Received from PTY channel, forwarded to PTY
- `on_agent_created`: Received via Hub channel
- `create_agent`: Send via Hub channel

**Channel management:**
- Hub channel connects on browser auth, stays connected for session lifetime
- PTY channels connect/disconnect as browser hops between views
- Each channel is its own encrypted websocket with separate IO
- **All channels use reliable mode** - handles backpressure, retries, and ensures delivery

**Contrast with TuiClient:**
- TUI uses local broadcast receivers (no network)
- Browser uses encrypted ActionCable channels (network)
- Same trait interface, different transport

---

## 4. Event & Command Definitions

### 4.1 Hub Events (broadcast to all clients)

```rust
#[derive(Debug, Clone)]
pub enum HubEvent {
    /// New agent was created.
    AgentCreated {
        agent_id: String,
        info: AgentInfo,
    },

    /// Agent was deleted.
    AgentDeleted {
        agent_id: String,
    },

    /// Agent status changed (idle, running, etc.).
    AgentStatusChanged {
        agent_id: String,
        status: AgentStatus,
    },

    /// Hub is shutting down.
    Shutdown,
}
```

### 4.2 Hub Commands (client → Hub)

```rust
#[derive(Debug)]
pub enum HubCommand {
    /// Create a new agent.
    CreateAgent {
        request: CreateAgentRequest,
        response_tx: oneshot::Sender<Result<AgentInfo, String>>,
    },

    /// Delete an agent.
    DeleteAgent {
        agent_id: String,
        response_tx: oneshot::Sender<Result<(), String>>,
    },

    /// List all agents.
    ListAgents {
        response_tx: oneshot::Sender<Vec<AgentInfo>>,
    },

    /// Get info for a specific agent.
    GetAgentInfo {
        agent_id: String,
        response_tx: oneshot::Sender<Option<AgentInfo>>,
    },
}
```

### 4.3 PTY Events (broadcast to connected clients)

```rust
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Raw output from PTY.
    Output(Vec<u8>),

    /// PTY was resized.
    Resized {
        rows: u16,
        cols: u16,
    },

    /// Process in PTY exited.
    ProcessExited {
        exit_code: Option<i32>,
    },

    /// Size ownership changed.
    OwnerChanged {
        new_owner: Option<ClientId>,
    },
}
```

---

## 5. Data Flow Diagrams

### 5.1 Client Connects to PTY Session

```
┌──────────┐                    ┌─────────────┐
│  Client  │                    │ PtySession  │
└────┬─────┘                    └──────┬──────┘
     │                                 │
     │  connect(client_id, dims)       │
     │────────────────────────────────>│
     │                                 │
     │                    ┌────────────┴────────────┐
     │                    │ Add to connected_clients │
     │                    │ Check if new size owner  │
     │                    │ Resize if needed         │
     │                    └────────────┬────────────┘
     │                                 │
     │  Ok(Receiver<PtyEvent>)         │
     │<────────────────────────────────│
     │                                 │
     │                    ┌────────────┴────────────┐
     │                    │ If resized, broadcast    │
     │                    │ PtyEvent::Resized        │
     │                    │ PtyEvent::OwnerChanged   │
     │                    └────────────┬────────────┘
     │                                 │
     │  PtyEvent::Resized (via rx)     │
     │<────────────────────────────────│
     │                                 │
```

### 5.2 PTY Output Flow

```
┌────────────┐     ┌─────────────┐     ┌───────────┐
│ PTY Reader │     │ PtySession  │     │  Clients  │
│   Thread   │     │             │     │ (N subs)  │
└─────┬──────┘     └──────┬──────┘     └─────┬─────┘
      │                   │                  │
      │ raw bytes         │                  │
      │──────────────────>│                  │
      │                   │                  │
      │         ┌─────────┴─────────┐        │
      │         │ Add to scrollback  │        │
      │         │ Broadcast output   │        │
      │         └─────────┬─────────┘        │
      │                   │                  │
      │                   │ PtyEvent::Output │
      │                   │─────────────────>│ (all subscribers)
      │                   │                  │
      │                   │         ┌────────┴────────┐
      │                   │         │ TUI: feed parser │
      │                   │         │ Browser: encrypt │
      │                   │         │   + send channel │
      │                   │         └────────┬────────┘
      │                   │                  │
```

### 5.3 Client Input Flow

```
┌───────────┐     ┌─────────────┐
│  Client   │     │ PtySession  │
└─────┬─────┘     └──────┬──────┘
      │                  │
      │ (user types)     │
      │                  │
      │ write_input(data)│
      │─────────────────>│
      │                  │
      │        ┌─────────┴─────────┐
      │        │ Write to PTY stdin │
      │        └─────────┬─────────┘
      │                  │
```

### 5.4 Hub Command Flow (Actor Pattern)

```
┌───────────┐     ┌──────────────┐     ┌───────┐
│  Client   │     │ Hub Command  │     │  Hub  │
│           │     │   Channel    │     │       │
└─────┬─────┘     └──────┬───────┘     └───┬───┘
      │                  │                 │
      │ CreateAgent {    │                 │
      │   request,       │                 │
      │   response_tx    │                 │
      │ }                │                 │
      │─────────────────>│                 │
      │                  │                 │
      │                  │ (Hub loop recv) │
      │                  │────────────────>│
      │                  │                 │
      │                  │      ┌──────────┴──────────┐
      │                  │      │ Process command      │
      │                  │      │ Create agent         │
      │                  │      │ Broadcast HubEvent   │
      │                  │      └──────────┬──────────┘
      │                  │                 │
      │  Result via      │                 │
      │  response_tx     │<────────────────│
      │<─────────────────│                 │
      │                  │                 │
```

---

## 6. Hot Paths

### 6.1 PTY Output (Highest Frequency)

```
PTY process writes → Reader thread reads → broadcast::send() →
  → TuiClient: vt100_parser.process()
  → BrowserClient: encrypt + channel.send()
```

**Critical for performance:**
- Reader thread must not block
- broadcast::send() is lock-free
- TuiClient parsing is sync, runs in client's thread
- BrowserClient encryption should be fast (already optimized)

### 6.2 User Input (High Frequency)

```
Keyboard/Browser → Client → pty.write_input() → PTY stdin
```

**Critical for performance:**
- Direct write, no broadcast needed
- TUI: sync write, immediate
- Browser: decrypt → write (channel already handles this)

### 6.3 Resize (Low Frequency)

```
Terminal resize → Client.resize() → pty.client_resized() →
  → If owner: pty.resize() → broadcast PtyEvent::Resized
```

**Not performance critical** - happens rarely.

### 6.4 Client Connect/Disconnect (Low Frequency)

```
Connect: Client → pty.connect() → update list → maybe resize
Disconnect: Client → pty.disconnect() → update list → maybe resize
```

**Not performance critical** - happens rarely.

---

## 7. Migration Plan

### Phase 1: Core Types & Events

1. [ ] Define `PtyEvent` enum in `cli/src/pty/events.rs`
2. [ ] Define `HubEvent` enum in `cli/src/hub/events.rs`
3. [ ] Define `HubCommand` enum in `cli/src/hub/commands.rs`
4. [ ] Define `ClientId` type (already exists, may need updates)
5. [ ] Define `ConnectedClient` struct for tracking in PtySession

### Phase 2: PtySession Refactor

1. [ ] Add `output_tx: broadcast::Sender<PtyEvent>` to PtySession
2. [ ] Add `connected_clients: Vec<ConnectedClient>` to PtySession
3. [ ] Implement `PtySession::connect()` → returns Receiver
4. [ ] Implement `PtySession::disconnect()`
5. [ ] Implement `PtySession::client_resized()`
6. [ ] Implement size owner logic (newest = owner)
7. [ ] Update PTY reader thread to broadcast instead of push to queue
8. [ ] Remove `raw_output_queue` (replaced by broadcast)
9. [ ] Keep scrollback buffer for history replay
10. [ ] Add tests for connect/disconnect/resize flows

### Phase 3: Client Trait Refactor

1. [ ] Define new `Client` trait with on_output, on_resize, etc.
2. [ ] Move `vt100_parser` from PtySession to TuiClient
3. [ ] Implement `Client` for TuiClient
4. [ ] Implement `Client` for BrowserClient
5. [ ] Add client event loop (receive from PtyEvent broadcast)
6. [ ] Update BrowserClient to manage PTY channels on connect/disconnect
7. [ ] Add tests for both client implementations

### Phase 4: Hub Refactor

1. [ ] Add `hub_event_tx: broadcast::Sender<HubEvent>` to Hub
2. [ ] Add `command_rx: mpsc::Receiver<HubCommand>` to Hub
3. [ ] Implement Hub command processing loop
4. [ ] Update agent CRUD to broadcast HubEvents
5. [ ] Remove old client→agent mapping from Hub
6. [ ] Update TuiClient to use command channel
7. [ ] Update BrowserClient to use command channel
8. [ ] Add tests for Hub command processing

### Phase 5: Integration & Cleanup

1. [ ] Wire up full flow: Hub ↔ Client ↔ PtySession
2. [ ] Update relay code to use new BrowserClient
3. [ ] Remove deprecated code paths
4. [ ] Update render.rs to use TuiClient's parser
5. [ ] Update input handling to use new flow
6. [ ] End-to-end integration tests
7. [ ] Performance testing (output throughput)
8. [ ] Documentation cleanup

---

## 8. Success Criteria

### Functional Requirements

- [ ] TUI can view any PTY session (cli or server) of any agent
- [ ] Browser can view any PTY session of any agent
- [ ] Multiple browsers can view same PTY simultaneously
- [ ] TUI and browser can view different PTYs of same agent
- [ ] Switching PTY views triggers proper resize
- [ ] Newest client to connect owns dimensions
- [ ] Disconnecting client triggers resize fallback
- [ ] All existing features continue to work

### Non-Functional Requirements

- [ ] No deadlocks or race conditions
- [ ] Output latency comparable to current implementation
- [ ] Memory usage stable under load
- [ ] Clean separation - components testable in isolation
- [ ] Easy to swap threads ↔ async tasks
- [ ] Code follows ms-rust guidelines

### Test Coverage

- [ ] Unit tests for PtySession connect/disconnect/resize logic
- [ ] Unit tests for Client trait implementations
- [ ] Unit tests for Hub command processing
- [ ] Integration tests for full flow
- [ ] Tests with multiple concurrent clients
- [ ] Tests for client disconnect scenarios

---

## 9. Appendix: Type Definitions Summary

```rust
// === Client Identity ===
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientId {
    Tui,
    Browser(String), // Signal identity key
}

// === PTY Session Types ===
#[derive(Debug, Clone)]
pub struct ConnectedClient {
    pub id: ClientId,
    pub dims: (u16, u16),
    pub connected_at: Instant,
}

#[derive(Debug, Clone)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Resized { rows: u16, cols: u16 },
    ProcessExited { exit_code: Option<i32> },
    OwnerChanged { new_owner: Option<ClientId> },
}

// === Hub Types ===
#[derive(Debug, Clone)]
pub enum HubEvent {
    AgentCreated { agent_id: String, info: AgentInfo },
    AgentDeleted { agent_id: String },
    AgentStatusChanged { agent_id: String, status: AgentStatus },
    Shutdown,
}

#[derive(Debug)]
pub enum HubCommand {
    CreateAgent { request: CreateAgentRequest, response_tx: oneshot::Sender<Result<AgentInfo, String>> },
    DeleteAgent { agent_id: String, response_tx: oneshot::Sender<Result<(), String>> },
    ListAgents { response_tx: oneshot::Sender<Vec<AgentInfo>> },
    GetAgentInfo { agent_id: String, response_tx: oneshot::Sender<Option<AgentInfo>> },
}

// === Client Trait ===
pub trait Client: Send {
    // Identity
    fn id(&self) -> &ClientId;
    fn dims(&self) -> (u16, u16);

    // PTY Event Handlers
    fn on_output(&mut self, data: &[u8]);
    fn on_resize(&mut self, rows: u16, cols: u16);
    fn on_process_exit(&mut self, exit_code: Option<i32>);
    fn on_owner_changed(&mut self, new_owner: Option<ClientId>);

    // Hub Event Handlers
    fn on_agent_created(&mut self, agent_id: &str, info: &AgentInfo);
    fn on_agent_deleted(&mut self, agent_id: &str);
    fn on_hub_shutdown(&mut self);

    // Actions
    fn send_input(&mut self, data: &[u8]);
    fn resize(&mut self, rows: u16, cols: u16);

    // Hub Commands
    fn create_agent(&self, request: CreateAgentRequest);
    fn delete_agent(&self, agent_id: &str);
    fn list_agents(&self);
}
```

---

## 10. Resolved Questions

1. **Scrollback on reconnect:** Full scrollback replay. PtySession keeps complete scrollback buffer.

2. **Multiple PTY views per client:** Architecture explicitly supports this - a client can connect to multiple PtySessions simultaneously. Current TUI/Browser implementations use one active PTY for simplicity, but that's a client-side choice, not an architectural limitation.

3. **Hub channel for TUI:** Yes, TUI uses Hub command channel just like Browser. TUI runs in its own thread. This ensures uniform testing and no special-casing.

4. **Lagged receivers:** Log and continue. Terminal state eventually converges.
