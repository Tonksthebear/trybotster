# Async Client Architecture Plan

**Status:** Technical Reference
**Created:** 2026-01-25
**Scope:** What changes to make clients run as independent async tokio tasks

---

## Table of Contents

1. [Current Architecture (As-Is)](#1-current-architecture-as-is)
2. [Blocking Inventory](#2-blocking-inventory)
3. [Type Constraints Blocking Async](#3-type-constraints-blocking-async)
4. [Hub Event Loop Transformation](#4-hub-event-loop-transformation)
5. [The Client Trait Problem](#5-the-client-trait-problem)
6. [Concrete Migration Path](#6-concrete-migration-path)
7. [What We Get](#7-what-we-get)

---

## 1. Current Architecture (As-Is)

### 1a. TUI Keyboard Input Flow

```
Thread: tui-runner                          Thread: main (Hub)
-----------------                          ------------------
User types
  -> crossterm::event::read()              (TuiRunner thread)
  -> TuiRunner::handle_input_event()       runner.rs:359
  -> process_event() returns InputResult   runner.rs:370
  -> InputResult::PtyInput(data)           runner.rs:374
  -> TuiRunner::handle_pty_input()         runner.rs:381
  -> request_tx.send(TuiRequest::SendInput{...})
                                           runner.rs:383-389
     |
     | tokio::sync::mpsc::UnboundedSender (non-blocking)
     v
                                           hub.clients.poll_all_requests()
                                             runner.rs:724 (in run_with_hub loop)
                                           -> ClientRegistry::poll_all_requests()
                                             registry.rs:92
                                           -> TuiClient::poll_requests()
                                             tui.rs:341
                                           -> TuiClient::handle_request(SendInput)
                                             tui.rs:371-374
                                           -> Client::send_input(agent_idx, pty_idx, data)
                                             mod.rs:384
                                           -> HubHandle::get_agent(idx) [reads HandleCache, non-blocking]
                                             hub_handle.rs:181
                                           -> PtyHandle::write_input_blocking(data)  <-- BLOCKING
                                             agent_handle.rs:280
                                           -> mpsc::Sender::blocking_send(PtyCommand::Input)
                                             agent_handle.rs:282
```

**Thread summary:**

| Step | Thread | Blocking? |
|------|--------|-----------|
| Keyboard read | tui-runner | Polls with 0ms timeout |
| `request_tx.send()` | tui-runner | Non-blocking (unbounded) |
| `poll_all_requests()` | main | Non-blocking (try_recv) |
| `handle_request()` | main | No |
| `send_input()` | main | No (HandleCache read) |
| `write_input_blocking()` | main | **YES** - `blocking_send` on tokio mpsc |

### 1b. Browser Input Flow

```
Thread: tokio task                         Thread: main (Hub)
-----------------                          ------------------
WebSocket message arrives
  -> spawn_pty_input_receiver()            browser.rs:625
     (async task on tokio runtime)
  -> receiver.recv().await                 browser.rs:640
  -> parse BrowserCommand::Input           browser.rs:671
  -> request_tx.send(BrowserRequest::SendInput{...})
                                           browser.rs:673-678
     |
     | tokio::sync::mpsc::UnboundedSender (non-blocking)
     v
                                           hub.clients.poll_all_requests()
                                             runner.rs:724
                                           -> BrowserClient::poll_requests()
                                             browser.rs:281
                                           -> BrowserClient::handle_request(SendInput)
                                             browser.rs:309-311
                                           -> Client::send_input()
                                             mod.rs:384
                                           -> PtyHandle::write_input_blocking()  <-- BLOCKING
                                             agent_handle.rs:280
```

**Thread summary:** Same as TUI, except the input source is a tokio task instead of a crossterm event. The blocking point is identical: `write_input_blocking()` on the main thread.

### 1c. Hub Main Event Loop (run_with_hub)

`cli/src/tui/runner.rs:717-743` - the main thread loop:

```rust
while !hub.quit && !shutdown_flag.load(Ordering::SeqCst) {
    // 1. Process commands from TuiRunner and other clients
    hub.process_commands();                          // mod.rs:849 - try_recv loop, non-blocking

    // 2. Poll client request channels (TuiClient, BrowserClient)
    hub.clients.poll_all_requests();                 // registry.rs:92 - calls poll_requests()
                                                     // on each client, which calls blocking
                                                     // PtyHandle methods

    if hub.quit { break; }

    // 3. Poll browser events (HubRelay - hub-level commands)
    browser::poll_events_headless(hub)?;             // Non-blocking

    // 4. Poll pending agents and progress events
    hub.poll_pending_agents();                       // std_mpsc::try_recv, non-blocking
    hub.poll_progress_events();                      // std_mpsc::try_recv, non-blocking

    // 5. Periodic tasks (polling, heartbeat, notifications)
    hub.tick();                                      // server_comms.rs:53 - mostly non-blocking

    thread::sleep(Duration::from_millis(16));        // 60 FPS tick
}
```

**Every call in the loop body:**

| Call | File:Line | Blocking? | Notes |
|------|-----------|-----------|-------|
| `process_commands()` | mod.rs:849 | No | `try_recv` loop on tokio mpsc |
| `poll_all_requests()` | registry.rs:92 | **YES** | Calls `write_input_blocking`, `resize_blocking`, `connect_blocking`, `disconnect_blocking` |
| `browser::poll_events_headless()` | relay/browser.rs | No | `try_recv` on channels |
| `poll_pending_agents()` | server_comms.rs:269 | No | `std_mpsc::try_recv` |
| `poll_progress_events()` | server_comms.rs:280 | No | `std_mpsc::try_recv` |
| `tick()` | server_comms.rs:53 | Mostly no | Background workers are non-blocking; fallback path blocks |
| `thread::sleep(16ms)` | - | Yes (intentional) | Rate limiter |

**The single pain point is `poll_all_requests()`.** It serializes all client request processing on the main thread, and each request can call blocking PtyHandle methods.

### 1d. Client Registration

`cli/src/hub/mod.rs:352-359` (Hub::new):

```rust
clients: {
    let mut registry = ClientRegistry::new();
    let (output_tx, _output_rx) = tokio::sync::mpsc::unbounded_channel();
    registry.register(Box::new(TuiClient::new(hub_handle_for_tui, output_tx, runtime_handle)));
    registry
},
```

TuiClient is created during `Hub::new()` and registered in `ClientRegistry`. A proper TuiClient with real channels is registered later in `register_tui_client_with_request_channel()` (`mod.rs:778`).

BrowserClient is created in `handle_client_connected()` (`client_handlers.rs:487-511`) when a browser handshake completes.

**Ownership:** Hub owns `ClientRegistry` (`pub clients: ClientRegistry`), which owns `HashMap<ClientId, Box<dyn Client>>`. Hub has `&mut` access to all clients.

---

## 2. Blocking Inventory

### 2a. Client Trait Methods

`cli/src/client/mod.rs:183-590`

| Method | Signature | Default Impl? | Blocking? | What blocks |
|--------|-----------|---------------|-----------|-------------|
| `hub_handle()` | `&self -> &HubHandle` | No (required) | No | Just returns ref |
| `as_any()` | `&self -> Option<&dyn Any>` | Yes (returns None) | No | |
| `as_any_mut()` | `&mut self -> Option<&mut dyn Any>` | Yes (returns None) | No | |
| `id()` | `&self -> &ClientId` | No (required) | No | Returns ref |
| `dims()` | `&self -> (u16, u16)` | No (required) | No | Returns copy |
| `set_dims()` | `&mut self, cols, rows` | No (required) | No | Updates local field |
| `connect_to_pty_with_handle()` | `&mut self, &AgentHandle, usize, usize -> Result` | No (required) | **YES** | Calls `pty.connect_blocking()` |
| `connect_to_pty()` | `&mut self, usize, usize -> Result` | Yes | **YES** | Calls `connect_to_pty_with_handle()` |
| `disconnect_from_pty()` | `&mut self, usize, usize` | No (required) | **YES** | Calls `pty.disconnect_blocking()` |
| `select_agent()` | `&mut self, usize -> Result<AgentMetadata>` | Yes | **YES** | Calls `connect_to_pty()` |
| `get_agents()` | `&self -> Vec<AgentInfo>` | Yes | **YES** | `command_tx.list_agents_blocking()` |
| `get_agent()` | `&self, usize -> Option<AgentHandle>` | Yes | No | Reads from HandleCache |
| `send_input()` | `&self, usize, usize, &[u8] -> Result` | Yes | **YES** | Calls `write_input_blocking()` |
| `resize_pty()` | `&self, usize, usize, u16, u16 -> Result` | Yes | **YES** | Calls `resize_blocking()` |
| `resize_pty_with_handle()` | `&self, &PtyHandle, u16, u16 -> Result` | Yes | **YES** | `pty.resize_blocking()` |
| `send_input_with_handle()` | `&self, &PtyHandle, &[u8] -> Result` | Yes | **YES** | `pty.write_input_blocking()` |
| `disconnect_from_pty_with_handle()` | `&mut self, &PtyHandle, usize, usize` | Yes | **YES** | `pty.disconnect_blocking()` |
| `agent_count()` | `&self -> usize` | Yes | **YES** | Calls `get_agents()` |
| `quit()` | `&self -> Result` | Yes | **YES** | `command_tx.quit_blocking()` |
| `list_worktrees()` | `&self -> Vec<(String, String)>` | Yes | No | Reads HandleCache |
| `get_connection_code()` | `&self -> Result<String>` | Yes | No | Reads HandleCache |
| `create_agent()` | `&self, CreateAgentRequest -> Result` | Yes | **YES** | `blocking_send` |
| `delete_agent()` | `&self, DeleteAgentRequest -> Result` | Yes | **YES** | `blocking_send` |
| `regenerate_connection_code()` | `&self -> Result` | Yes | **YES** | `dispatch_action_blocking()` |
| `copy_connection_url()` | `&self -> Result` | Yes | **YES** | `dispatch_action_blocking()` |

**Summary:** 16 of 25 methods are blocking. The non-blocking ones are: `hub_handle`, `as_any`, `as_any_mut`, `id`, `dims`, `set_dims`, `get_agent`, `list_worktrees`, `get_connection_code`.

### 2b. HubHandle Methods

`cli/src/hub/hub_handle.rs`

| Method | Blocking? | Mechanism |
|--------|-----------|-----------|
| `get_agents()` | **YES** | `command_tx.list_agents_blocking()` - roundtrip through Hub command channel |
| `get_agent(idx)` | No | Reads from `HandleCache` (RwLock read) |
| `create_agent()` | **YES** | `inner().blocking_send(cmd)` |
| `delete_agent()` | **YES** | `inner().blocking_send(cmd)` |
| `delete_agent_with_worktree()` | **YES** | `inner().blocking_send(cmd)` |
| `quit()` | **YES** | `command_tx.quit_blocking()` |
| `is_closed()` | No | `command_tx.is_closed()` |
| `agent_count()` | **YES** | Calls `get_agents()` |
| `has_agents()` | **YES** | Calls `get_agents()` |
| `list_worktrees()` | No | Reads from `HandleCache` |
| `dispatch_action()` | **YES** | `command_tx.dispatch_action_blocking()` |
| `get_connection_code()` | No | Reads from `HandleCache` |
| `refresh_connection_code()` | No | `command_tx.try_send(cmd)` |
| `crypto_service()` | **YES** | `command_tx.get_crypto_service_blocking()` |
| `server_hub_id()` | **YES** | `command_tx.get_server_hub_id_blocking()` |
| `server_url()` | **YES** | `command_tx.get_server_url_blocking()` |
| `api_key()` | **YES** | `command_tx.get_api_key_blocking()` |
| `tokio_runtime()` | **YES** | `command_tx.get_tokio_runtime_blocking()` |

**Async equivalents:** HubHandle does not currently expose async methods. However, the underlying `tokio::sync::mpsc::Sender` supports `.send().await` natively. Adding async versions would be straightforward -- replace `blocking_send` with `.send().await`.

### 2c. PtyHandle Methods

`cli/src/hub/agent_handle.rs:228-393`

| Blocking Method | Async Equivalent | Channel |
|-----------------|------------------|---------|
| `write_input_blocking(&self, &[u8])` | `write_input(&self, &[u8]).await` | `mpsc::Sender<PtyCommand>` |
| `resize_blocking(&self, ClientId, u16, u16)` | `resize(&self, ClientId, u16, u16).await` | `mpsc::Sender<PtyCommand>` |
| `connect_blocking(&self, ClientId, (u16,u16))` | `connect(&self, ClientId, (u16,u16)).await` | `mpsc::Sender<PtyCommand>` + oneshot response |
| `disconnect_blocking(&self, ClientId)` | `disconnect(&self, ClientId).await` | `mpsc::Sender<PtyCommand>` |

**Every blocking method already has an async counterpart.** The channel type is `tokio::sync::mpsc::Sender<PtyCommand>`, which natively supports both `blocking_send()` and `.send().await`. No new channel infrastructure needed.

---

## 3. Type Constraints Blocking Async

`tokio::spawn` requires `Send + 'static`. Let's audit each client.

### 3a. TuiClient

`cli/src/client/tui.rs:223-258`

```rust
pub struct TuiClient {
    hub_handle: HubHandle,                              // Clone + Send + Sync
    runtime: Handle,                                     // Clone + Send + Sync
    id: ClientId,                                        // Clone + Send + Sync
    dims: (u16, u16),                                    // Copy + Send + Sync
    output_sink: UnboundedSender<TuiOutput>,             // Clone + Send + Sync
    output_task: Option<JoinHandle<()>>,                 // Send (JoinHandle is Send)
    request_rx: Option<UnboundedReceiver<TuiRequest>>,   // Send (NOT Sync)
}
```

| Field | Send | Sync | Notes |
|-------|------|------|-------|
| `hub_handle: HubHandle` | Yes | Yes | Contains Arc<HandleCache> and mpsc::Sender |
| `runtime: Handle` | Yes | Yes | tokio runtime handle |
| `id: ClientId` | Yes | Yes | Enum of String |
| `dims: (u16, u16)` | Yes | Yes | Copy type |
| `output_sink: UnboundedSender<TuiOutput>` | Yes | Yes | |
| `output_task: Option<JoinHandle<()>>` | Yes | Yes | JoinHandle<T> is Send+Sync when T: Send |
| `request_rx: Option<UnboundedReceiver<TuiRequest>>` | Yes | No | Receiver is not Sync |

**TuiClient is `Send` but NOT `Sync`.** This is fine for `tokio::spawn` which requires `Send + 'static`. The `'static` requirement means no borrowed references, which TuiClient satisfies (all owned fields).

**No wrapping needed for `tokio::spawn`.** TuiClient can be moved into a task directly.

### 3b. BrowserClient

`cli/src/client/browser.rs:167-208`

```rust
pub struct BrowserClient {
    hub_handle: HubHandle,                                          // Send + Sync
    runtime: Handle,                                                 // Send + Sync
    id: ClientId,                                                    // Send + Sync
    dims: (u16, u16),                                                // Copy
    identity: String,                                                // Send + Sync
    config: BrowserClientConfig,                                     // Send + Sync (all fields are)
    terminal_channels: HashMap<(usize, usize), TerminalChannel>,    // Send (not Sync)
    request_tx: UnboundedSender<BrowserRequest>,                     // Send + Sync
    request_rx: UnboundedReceiver<BrowserRequest>,                   // Send (not Sync)
}
```

| Field | Send | Sync | Notes |
|-------|------|------|-------|
| `hub_handle` | Yes | Yes | |
| `runtime` | Yes | Yes | |
| `id` | Yes | Yes | |
| `dims` | Yes | Yes | |
| `identity` | Yes | Yes | |
| `config` | Yes | Yes | BrowserClientConfig has CryptoServiceHandle (Send+Sync) |
| `terminal_channels` | Yes | No | Contains JoinHandles and ActionCableChannel |
| `request_tx` | Yes | Yes | |
| `request_rx` | Yes | No | |

**BrowserClient is `Send` but NOT `Sync`.** Same situation as TuiClient -- works for `tokio::spawn`.

### 3c. The Client Trait

`cli/src/client/mod.rs:183`

```rust
pub trait Client: Send {
```

The trait requires `Send` only. It is **object-safe** today (no generic methods, no `Self` in return types, no associated types). It uses `&mut self` on several methods.

**Key constraint for async:** You cannot have `&mut self` across `.await` points in an async method without wrapping in `Mutex`. But since each client would own itself exclusively in its task, `&mut self` is fine within a single task -- no sharing needed.

**Object safety with async methods:** `async fn` in traits requires either:
- `async-trait` crate (boxes the future, works with dyn)
- Native async fn in traits (stabilized in Rust 1.75, but dyn dispatch still requires `#[trait_variant::make]` or boxing)

**The Client trait could have async methods**, but `dyn Client` with async methods requires boxing (via `async-trait` or manual `Box<dyn Future>`).

---

## 4. Hub Event Loop Transformation

### 4a. Current Loop Body

`cli/src/tui/runner.rs:717-743`:

```
1. hub.process_commands()           -- drain Hub command channel (non-blocking)
2. hub.clients.poll_all_requests()  -- drain client request channels, execute blocking PTY ops
3. browser::poll_events_headless()  -- drain browser relay events (non-blocking)
4. hub.poll_pending_agents()        -- check background agent creation results
5. hub.poll_progress_events()       -- check agent creation progress
6. hub.tick()                       -- periodic tasks (polling, heartbeat)
7. thread::sleep(16ms)              -- rate limiter
```

### 4b. Target: tokio::select! Loop

The loop becomes async, selecting over multiple event sources:

```rust
loop {
    tokio::select! {
        // Hub command channel
        Some(cmd) = hub.command_rx.recv() => {
            hub.handle_command(cmd);
        }

        // Hub events from broadcast (agent created, deleted, etc.)
        // Only if Hub needs to react to its own events

        // Client completion signals (a client task exited)
        Some(result) = client_completions.recv() => {
            hub.handle_client_exit(result);
        }

        // Pending agent results (background creation finished)
        result = pending_agent_rx.recv() => {
            hub.handle_pending_agent(result);
        }

        // Progress events
        progress = progress_rx.recv() => {
            hub.handle_progress(progress);
        }

        // Browser relay events
        event = browser_rx.recv() => {
            hub.handle_browser_event(event);
        }

        // Periodic tick (replaces thread::sleep)
        _ = tick_interval.tick() => {
            hub.tick();
        }
    }
}
```

**What disappears:** `hub.clients.poll_all_requests()` is eliminated entirely. Client request processing moves into each client's own async task.

### 4c. Client Ownership Model Change

**Current:** Hub owns clients via `ClientRegistry` (HashMap of `Box<dyn Client>`).

```
Hub
  └── clients: ClientRegistry
        └── HashMap<ClientId, Box<dyn Client>>
              ├── ClientId::Tui -> Box<TuiClient>
              └── ClientId::Browser("abc") -> Box<BrowserClient>
```

**Target:** Each client is `tokio::spawn`ed. Hub communicates via channels.

```
Hub
  └── client_handles: HashMap<ClientId, ClientTaskHandle>
        ├── ClientId::Tui -> ClientTaskHandle { cmd_tx, join_handle }
        └── ClientId::Browser("abc") -> ClientTaskHandle { cmd_tx, join_handle }

tokio task: TuiClient
  └── owns: TuiClient, cmd_rx
  └── loop: select! { request from request_rx, cmd from cmd_rx }

tokio task: BrowserClient
  └── owns: BrowserClient, cmd_rx
  └── loop: select! { request from request_rx, cmd from cmd_rx }
```

### 4d. Every Place Hub Calls Client Methods Directly

Searched for: `clients.get`, `clients.get_mut`, `get_tui_mut`, `get_tui`, `clients.iter`, `clients.iter_mut`

| Location | Method Called | What It Does |
|----------|-------------|--------------|
| `client_handlers.rs:128-183` `handle_resize_for_client` | `clients.get(&id)` then `clients.get_mut(&id)` then `clients.get(&id)` | Reads `connected_ptys()`, calls `set_dims()`, calls `resize_pty_with_handle()` |
| `client_handlers.rs:457-463` `handle_delete_agent_for_client` | `clients.iter_mut()` | Calls `disconnect_from_pty(idx, 0)` and `disconnect_from_pty(idx, 1)` on ALL clients |
| `client_handlers.rs:509` `handle_client_connected` | `clients.register()` | Registers new BrowserClient |
| `client_handlers.rs:523` `handle_client_disconnected` | `clients.unregister()` | Removes client |
| `client_handlers.rs:368` `spawn_agent_sync` | `clients.get(&client_id)` | Reads `dims()` for PTY spawn |
| `mod.rs:692-694` `Hub::new` | `clients.get_mut(&Tui)` (in `run_with_hub`) | `set_dims()` on TuiClient |
| `mod.rs:992-996` `handle_command(BrowserPtyInput)` | `clients.get(&client_id)` | `send_input()` |
| `registry.rs:92-102` `poll_all_requests` | `as_any_mut()` + downcast | `poll_requests()` on each client |
| `hub/actions/mod.rs:449` | (indirect via `send_agent_list_to`) | Sends agent list to client |
| `hub/actions/mod.rs:456` | (indirect via `send_worktree_list_to`) | Sends worktree list to client |

### 4e. What Replaces Direct Method Calls

For each direct call site, the replacement:

| Current Direct Call | Replacement | New Channel Message |
|--------------------|-------------|---------------------|
| `client.set_dims(cols, rows)` | Send via client's command channel | `ClientCmd::SetDims { cols, rows }` |
| `client.resize_pty_with_handle(pty, rows, cols)` | Client does this internally when it receives SetDims | N/A (client handles PTY resize itself) |
| `client.disconnect_from_pty(idx, pty_idx)` | Send via client's command channel | `ClientCmd::DisconnectFromPty { agent_index, pty_index }` |
| `client.dims()` | Cache dims in Hub's `ClientTaskHandle` or send query | `ClientCmd::GetDims { response_tx }` or cached |
| `client.send_input(...)` (BrowserPtyInput) | Already goes through BrowserRequest channel | No change needed |
| `clients.register(Box::new(client))` | `tokio::spawn(client_task)`, store handle | Hub spawns the task |
| `clients.unregister(&id)` | Drop the `ClientTaskHandle`, abort the task | Task exits cleanly |
| `poll_requests()` | Each client runs its own select loop | Eliminated from Hub |

**New `ClientCmd` enum** (replaces Hub calling methods on clients directly):

```rust
enum ClientCmd {
    SetDims { cols: u16, rows: u16 },
    DisconnectFromPty { agent_index: usize, pty_index: usize },
    ConnectToPty { agent_handle: AgentHandle, agent_index: usize, pty_index: usize },
    Shutdown,
}
```

---

## 5. The Client Trait Problem

### Current: `&mut self` Everywhere

The Client trait has `&mut self` on: `set_dims`, `connect_to_pty_with_handle`, `connect_to_pty`, `disconnect_from_pty`, `select_agent`, `disconnect_from_pty_with_handle`.

And `&self` on: `hub_handle`, `id`, `dims`, `send_input`, `resize_pty`, all hub management methods.

### Why This Matters for Async

In an async task that exclusively owns the client, `&mut self` is fine -- there's no contention. The problem is Hub's current pattern of reaching into the registry and calling methods. With clients in their own tasks, Hub can't call methods at all. So the trait becomes irrelevant for the Hub->Client direction.

### Option A: Async Trait with Concrete Types

Replace blocking calls with async equivalents. No `dyn Client` dispatch.

```rust
// Each client type gets its own async run loop
impl TuiClient {
    async fn run(mut self, mut cmd_rx: mpsc::Receiver<ClientCmd>) {
        loop {
            tokio::select! {
                Some(request) = self.request_rx.recv() => {
                    self.handle_request_async(request).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    self.handle_cmd(cmd).await;
                }
            }
        }
    }

    async fn handle_request_async(&mut self, request: TuiRequest) {
        match request {
            TuiRequest::SendInput { agent_index, pty_index, data } => {
                // Use async PtyHandle methods instead of _blocking
                if let Some(agent) = self.hub_handle.get_agent(agent_index) {
                    if let Some(pty) = agent.get_pty(pty_index) {
                        let _ = pty.write_input(&data).await;
                    }
                }
            }
            // ...
        }
    }
}
```

**Pros:** Clean, no boxing overhead, full async.
**Cons:** Loses the unified Client trait for polymorphic handling. Hub must handle TUI and Browser separately.

### Option B: Channel-Based Protocol (TuiRequest/BrowserRequest Already Exist)

The request enums already cover most operations. Extend them and replace the trait entirely for the Hub->Client direction.

**What TuiRequest already covers:**

| Operation | TuiRequest Variant | Status |
|-----------|-------------------|--------|
| Send input | `SendInput` | Exists |
| Resize + dims | `SetDims` | Exists |
| Select agent | `SelectAgent` | Exists |
| Connect to PTY | `ConnectToPty` | Exists |
| Disconnect from PTY | `DisconnectFromPty` | Exists |
| Quit | `Quit` | Exists |
| List worktrees | `ListWorktrees` | Exists |
| Get connection code | `GetConnectionCode` | Exists |
| Create agent | `CreateAgent` | Exists |
| Delete agent | `DeleteAgent` | Exists |
| Regenerate code | `RegenerateConnectionCode` | Exists |
| Copy URL | `CopyConnectionUrl` | Exists |

**What's MISSING from TuiRequest (things Hub calls directly on TuiClient):**

| Hub Direct Call | What to Add |
|----------------|-------------|
| `clear_connection()` | `TuiRequest::ClearConnection` |
| `set_dims()` (without PTY propagation) | `TuiRequest::UpdateDims { cols, rows }` (no PTY indices) |

**What BrowserRequest already covers:**

| Operation | BrowserRequest Variant | Status |
|-----------|----------------------|--------|
| Send input | `SendInput` | Exists |
| Resize | `Resize` | Exists |

**What's MISSING from BrowserRequest (things Hub calls directly on BrowserClient):**

| Hub Direct Call | What to Add |
|----------------|-------------|
| `disconnect_from_pty(idx, pty_idx)` | `BrowserRequest::DisconnectFromPty { agent_index, pty_index }` |
| `set_dims(cols, rows)` | `BrowserRequest::SetDims { cols, rows }` |
| `connected_ptys()` | Either cache in Hub or add `BrowserRequest::GetConnectedPtys { response_tx }` |
| `connect_to_pty_with_handle()` | `BrowserRequest::ConnectToPty { agent_handle, agent_index, pty_index }` |

**This is the recommended approach.** The infrastructure is 90% built. TuiRequest and BrowserRequest already exist and handle all the high-frequency operations. Adding the missing variants is minimal work.

### Option C: Arc<Mutex<dyn Client>>

Wrap each client in `Arc<Mutex<dyn Client>>`, call blocking methods from async context via `spawn_blocking`.

**Pros:** Minimal code change.
**Cons:** Defeats the purpose. Blocking methods still block a thread pool thread. Mutex contention between Hub and client tasks. This is a hack.

### Recommendation: Option B

The channel protocol already exists. Extend it with the missing variants, then each client's task just runs a `select!` loop over its request channel plus a Hub command channel.

---

## 6. Concrete Migration Path

### Step 1: Add Missing Request Variants

**Files changed:**
- `cli/src/client/tui.rs` -- add `TuiRequest::ClearConnection`, `TuiRequest::UpdateDims`
- `cli/src/client/browser.rs` -- add `BrowserRequest::DisconnectFromPty`, `BrowserRequest::SetDims`, `BrowserRequest::ConnectToPty`

**Conceptual diff:**

```rust
// tui.rs - TuiRequest enum
pub enum TuiRequest {
    // ... existing variants ...

    /// Clear connection state (no PTY notification).
    /// Replaces TuiClient::clear_connection() direct call.
    ClearConnection,

    /// Update dims without PTY propagation.
    /// Used when Hub sets initial dims before any agent is selected.
    UpdateDims { cols: u16, rows: u16 },
}

// browser.rs - BrowserRequest enum
pub enum BrowserRequest {
    // ... existing variants ...

    /// Disconnect from a specific PTY.
    DisconnectFromPty { agent_index: usize, pty_index: usize },

    /// Update terminal dimensions.
    SetDims { cols: u16, rows: u16 },

    /// Connect to a PTY (with agent handle for setup).
    ConnectToPty { agent_index: usize, pty_index: usize },
}
```

**Handle the new variants in each client's `handle_request()`.**

**Tests:** Existing unit tests cover the pattern. Add tests for new variants matching `test_poll_requests_set_dims` style.

**Risk:** Low. Additive change, existing behavior unchanged.

### Step 2: Add Async Methods to Client Trait (or Alongside)

**Files changed:**
- `cli/src/client/mod.rs` -- add async methods alongside blocking ones
- OR create separate async free functions that take `&PtyHandle`

**Conceptual diff:**

```rust
// In client/mod.rs or as standalone functions
pub async fn send_input_async(pty: &PtyHandle, data: &[u8]) -> Result<(), String> {
    pty.write_input(data).await
}

pub async fn resize_pty_async(pty: &PtyHandle, client_id: ClientId, rows: u16, cols: u16) -> Result<(), String> {
    pty.resize(client_id, rows, cols).await
}

pub async fn connect_async(pty: &PtyHandle, client_id: ClientId, dims: (u16, u16)) -> Result<Vec<u8>, String> {
    pty.connect(client_id, dims).await
}

pub async fn disconnect_async(pty: &PtyHandle, client_id: ClientId) -> Result<(), String> {
    pty.disconnect(client_id).await
}
```

**Tests:** The existing `PtyHandle` async tests (`agent_handle.rs:507-614`) already cover the async methods. No new tests needed for the handle layer.

**Risk:** Low. Purely additive.

### Step 3: Convert TuiClient::handle_request to Async

**Files changed:**
- `cli/src/client/tui.rs` -- `handle_request` becomes `async fn handle_request_async`

**Conceptual diff:**

```rust
impl TuiClient {
    async fn handle_request_async(&mut self, request: TuiRequest) {
        match request {
            TuiRequest::SendInput { agent_index, pty_index, data } => {
                if let Some(agent) = self.hub_handle.get_agent(agent_index) {
                    if let Some(pty) = agent.get_pty(pty_index) {
                        if let Err(e) = pty.write_input(&data).await {
                            log::error!("Failed to send input: {}", e);
                        }
                    }
                }
            }
            TuiRequest::SetDims { agent_index, pty_index, cols, rows } => {
                self.dims = (cols, rows);
                if let Some(agent) = self.hub_handle.get_agent(agent_index) {
                    if let Some(pty) = agent.get_pty(pty_index) {
                        if let Err(e) = pty.resize(self.id.clone(), rows, cols).await {
                            log::error!("Failed to resize: {}", e);
                        }
                    }
                }
            }
            TuiRequest::SelectAgent { index, response_tx } => {
                let result = self.select_agent_async(index).await;
                let _ = response_tx.send(result.ok().map(|m| TuiAgentMetadata {
                    agent_id: m.agent_id,
                    agent_index: m.agent_index,
                    has_server_pty: m.has_server_pty,
                }));
            }
            // ... rest of variants converted similarly
        }
    }
}
```

**Key change in `connect_to_pty_with_handle`:** Replace `pty_handle.connect_blocking()` with `pty_handle.connect().await`.

**Tests:** Integration tests in `tui.rs::integration` module will need updating. They currently call `poll_requests()` synchronously. They'd need to either:
- Use `#[tokio::test]` and call the async method directly
- Or keep `poll_requests()` as a sync wrapper during migration

**Risk:** Medium. The `select_agent` -> `connect_blocking` path is the most complex. `connect_blocking` sends a command and waits for a oneshot response. The async version sends via `.send().await` and awaits the oneshot `.await`. The TuiRunner side still calls `blocking_recv()` on its oneshot, which is fine (TuiRunner is on a separate thread).

### Step 4: Convert BrowserClient::handle_request to Async

**Files changed:**
- `cli/src/client/browser.rs` -- same pattern as Step 3

**Diff is simpler** since BrowserRequest only has `SendInput` and `Resize`, plus the new variants from Step 1.

**Risk:** Low. BrowserClient already does most heavy lifting in async tasks (output forwarder, input receiver).

### Step 5: Create Client Task Runners

**Files changed:**
- `cli/src/client/tui.rs` -- add `async fn run_task(self, cmd_rx)`
- `cli/src/client/browser.rs` -- add `async fn run_task(self, cmd_rx)`

**Conceptual diff:**

```rust
// tui.rs
impl TuiClient {
    /// Run TuiClient as an independent async task.
    pub async fn run_task(
        mut self,
        mut cmd_rx: mpsc::Receiver<ClientCmd>,
    ) {
        let Some(mut request_rx) = self.request_rx.take() else {
            log::error!("TuiClient has no request receiver");
            return;
        };

        loop {
            tokio::select! {
                Some(request) = request_rx.recv() => {
                    self.handle_request_async(request).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        ClientCmd::SetDims { cols, rows } => {
                            self.dims = (cols, rows);
                        }
                        ClientCmd::DisconnectFromPty { agent_index, pty_index } => {
                            self.disconnect_from_pty_async(agent_index, pty_index).await;
                        }
                        ClientCmd::Shutdown => break,
                    }
                }
                else => break,
            }
        }
    }
}
```

**Tests:** New integration test: spawn `run_task` in a tokio test, send requests via channel, verify PTY commands arrive.

**Risk:** Medium. This is where ownership moves. TuiClient is consumed by `run_task`. Hub can no longer call methods on it directly.

### Step 6: Replace ClientRegistry with Task Handles

**Files changed:**
- `cli/src/client/registry.rs` -- replace `HashMap<ClientId, Box<dyn Client>>` with `HashMap<ClientId, ClientTaskHandle>`
- `cli/src/hub/mod.rs` -- update `Hub.clients` type, update registration flow
- `cli/src/hub/actions/client_handlers.rs` -- replace direct method calls with channel sends
- `cli/src/tui/runner.rs` -- update `run_with_hub` to spawn TuiClient task

**New type:**

```rust
pub struct ClientTaskHandle {
    pub cmd_tx: mpsc::Sender<ClientCmd>,
    pub join_handle: tokio::task::JoinHandle<()>,
    pub cached_dims: (u16, u16),  // avoid round-trip for dims queries
}
```

**Conceptual diff for `run_with_hub`:**

```rust
// Before (runner.rs:717-743):
hub.clients.poll_all_requests();  // REMOVED

// After:
// TuiClient is spawned as a tokio task during setup
let (tui_cmd_tx, tui_cmd_rx) = mpsc::channel(64);
let tui_client = TuiClient::new(hub_handle, output_tx, runtime_handle);
tui_client.set_request_receiver(request_rx);
let tui_join = hub.tokio_runtime.spawn(tui_client.run_task(tui_cmd_rx));

hub.client_handles.insert(ClientId::Tui, ClientTaskHandle {
    cmd_tx: tui_cmd_tx,
    join_handle: tui_join,
    cached_dims: (inner_cols, inner_rows),
});
```

**Conceptual diff for client_handlers.rs:**

```rust
// Before:
if let Some(client) = hub.clients.get_mut(&client_id) {
    client.set_dims(cols, rows);
}

// After:
if let Some(handle) = hub.client_handles.get(&client_id) {
    let _ = handle.cmd_tx.try_send(ClientCmd::SetDims { cols, rows });
    // Update cached dims
}
```

**Tests:** All integration tests in `tui.rs::integration` and `browser.rs::integration` need rewriting. They currently access Hub's client registry directly. With async clients, tests would send via channels and verify results.

**Risk:** HIGH. This is the largest single change. Every call site that touches `hub.clients` must be updated. The test suite needs significant rework. However, the system works after this step -- it's the "flip the switch" moment.

### Step 7: Convert Hub Event Loop to Async

**Files changed:**
- `cli/src/tui/runner.rs` -- `run_with_hub` becomes async or uses `block_on`
- `cli/src/hub/run.rs` -- `run_headless_loop` becomes async
- `cli/src/hub/mod.rs` -- `Hub.command_rx` processing moves into `select!`

**Conceptual diff:**

```rust
// run_with_hub becomes:
pub fn run_with_hub(hub: &mut Hub, terminal: ..., shutdown_flag: &AtomicBool) -> Result<()> {
    // ... setup TuiRunner thread (unchanged) ...
    // ... spawn TuiClient task ...

    // Main thread runs the async hub loop on the tokio runtime
    hub.tokio_runtime.block_on(async {
        let mut tick = tokio::time::interval(Duration::from_millis(16));

        loop {
            tokio::select! {
                Some(cmd) = hub.command_rx.recv() => {
                    hub.handle_command(cmd);
                }
                // Browser events, pending agents, etc. via channels
                _ = tick.tick() => {
                    hub.tick();
                    if hub.quit || shutdown_flag.load(Ordering::SeqCst) {
                        break;
                    }
                }
            }
        }
    });

    // ... shutdown ...
}
```

**Tests:** Hub-level tests that call `process_commands()` directly would need updating to use the async loop pattern.

**Risk:** Medium. The Hub itself stays synchronous (`&mut Hub`) -- only the event loop scheduling changes. The main thread blocks on `block_on()`, which is the current behavior (it blocks on `thread::sleep` today).

---

## 7. What We Get

### Benefits

**Concurrency:** Multiple browser clients can process requests simultaneously. Currently, `poll_all_requests()` iterates all clients sequentially on the main thread. With async tasks, each client processes its own requests independently.

**Non-blocking PTY operations:** `connect_blocking()` on one client (which waits for a oneshot response from the PTY command processor) no longer blocks all other clients. Today, if TuiClient calls `connect_blocking()` during `poll_all_requests()`, no other client's requests are processed until it returns.

**Free Hub loop:** The Hub event loop can process commands, browser events, and periodic tasks while clients independently handle PTY I/O. Today, a slow `write_input_blocking()` delays the entire loop tick.

**Natural async flow:** PtyHandle already has async methods. The blocking wrappers exist only because clients run on Hub's synchronous thread. Removing this indirection simplifies the code.

### Costs

**Channel indirection:** Hub can no longer call `client.dims()` or `client.set_dims()` directly. Every Hub->Client communication goes through `ClientCmd` channel sends. This adds latency (channel send + task wakeup) to what was previously a direct field read/write.

Mitigation: Cache frequently-read values (like `dims`) in `ClientTaskHandle` on Hub's side.

**Testing complexity:** Async tests require `#[tokio::test]`, channel setup, and await-based assertions. The current synchronous integration tests (which directly call `poll_requests()` and check state) are simpler to write and debug.

Mitigation: Keep a synchronous `poll_requests()` method alongside the async task runner for unit testing individual request handling.

**Error propagation:** With direct method calls, errors are returned immediately. With channels, errors are logged by the client task and Hub never sees them. If Hub needs to know about errors (e.g., to show UI feedback), it needs a response channel back.

Mitigation: For operations where Hub needs the result, use `ClientCmd` variants with a oneshot response channel.

**Lifetime of spawned tasks:** Hub must track task join handles and clean up on shutdown. If a client task panics, Hub needs to detect it and clean up. This is new failure mode that doesn't exist with direct ownership.

Mitigation: `tokio::select!` over join handles. On panic, log and remove the client handle.

### Migration Risk Assessment

| Step | Risk | Independently Shippable | Rollback Difficulty |
|------|------|------------------------|-------------------|
| 1. Add missing request variants | Low | Yes | Trivial |
| 2. Add async methods | Low | Yes | Trivial |
| 3. Convert TuiClient handle_request | Medium | Yes (keep sync wrapper) | Easy |
| 4. Convert BrowserClient handle_request | Low | Yes | Easy |
| 5. Create client task runners | Medium | Yes (unused until Step 6) | Easy |
| 6. Replace ClientRegistry | HIGH | Yes (but big diff) | Hard |
| 7. Convert Hub loop to async | Medium | Yes | Medium |

The critical path is Step 6. Steps 1-5 are all purely additive and can be shipped one at a time. Step 6 is the breaking change where the ownership model flips. Step 7 is a natural follow-on.

---

## Appendix: File Reference

| File | Role |
|------|------|
| `cli/src/client/mod.rs` | Client trait definition, ClientId enum |
| `cli/src/client/tui.rs` | TuiClient, TuiRequest, TuiOutput, poll_requests() |
| `cli/src/client/browser.rs` | BrowserClient, BrowserRequest, poll_requests() |
| `cli/src/client/registry.rs` | ClientRegistry (HashMap wrapper), poll_all_requests() |
| `cli/src/client/types.rs` | CreateAgentRequest, DeleteAgentRequest, Response |
| `cli/src/hub/mod.rs` | Hub struct, process_commands(), handle_command() |
| `cli/src/hub/hub_handle.rs` | HubHandle (thread-safe client API) |
| `cli/src/hub/agent_handle.rs` | AgentHandle, PtyHandle (blocking + async methods) |
| `cli/src/hub/handle_cache.rs` | HandleCache (RwLock-based agent handle cache) |
| `cli/src/hub/run.rs` | run_headless_loop() |
| `cli/src/hub/commands.rs` | HubCommand enum, HubCommandSender |
| `cli/src/hub/actions/mod.rs` | HubAction enum, dispatch() |
| `cli/src/hub/actions/client_handlers.rs` | handle_resize_for_client, handle_delete_agent, handle_client_connected/disconnected |
| `cli/src/tui/runner.rs` | TuiRunner, run_with_hub() (main event loop) |
| `cli/src/agent/pty/commands.rs` | PtyCommand enum |
