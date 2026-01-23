# Client Refactor Verification Spec

**Status:** Audit Complete
**Date:** 2026-01-21
**Purpose:** Document all architectural violations and test cases needed to verify the refactor

---

## 1. Architectural Violations Found

### 1.1 CRITICAL: Misplaced Files

| File | Current Location | Correct Location | Lines | Issue |
|------|------------------|------------------|-------|-------|
| `scroll.rs` | `agent/` | `client/` or `tui/` | 178 | View state (scroll offset) in Agent layer |
| `screen.rs` | `agent/` | `tui/` or `relay/` | 277 | Rendering logic (`render_screen_as_ansi`, `compute_screen_hash`) in Agent |
| `menu.rs` | `hub/` | `tui/` | 275 | TUI menu building (`build_menu`, `MenuAction`) in Hub layer |

**Impact:** Agent layer has UI concerns. Hub has presentation logic. Violates separation of concerns.

---

### 1.2 CRITICAL: Hub Imports TUI Directly

**Location:** `cli/src/hub/run.rs:45-46`
```rust
use crate::tui::layout::terminal_widget_inner_area;
use crate::tui::runner::TuiRunner;
```

**Issue:** Hub (orchestration layer) directly instantiates TuiRunner (UI layer). This creates:
- Hard compile-time dependency on TUI
- Can't run Hub headless without TUI code
- Violates dependency inversion principle

**Fix:** Hub should receive a trait object or be started by main.rs with TuiRunner injected.

---

### 1.3 CRITICAL: BrowserClient Channels Never Wired

**Location:** `cli/src/hub/actions.rs:1023-1032`

BrowserClient has correct fields:
```rust
hub_channel: Option<ActionCableChannel>,        // NEVER SET
pty_channels: HashMap<String, ActionCableChannel>,  // ALWAYS EMPTY
```

**Missing code in `handle_client_connected()`:**
- Never calls `browser_client.set_hub_channel()`
- Never creates PTY channels for viewing agents
- Never calls `browser_client.connect_pty_channel()`

**Impact:** Terminal output cannot reach browsers. The data pipe is completely disconnected.

---

### 1.4 HIGH: Client Trait Missing Hub Commands

**Design doc specifies** Client trait should have:
```rust
fn create_agent(&self, request: CreateAgentRequest);
fn delete_agent(&self, agent_id: &str);
fn list_agents(&self);
```

**Current implementation:** These are on `HubCommandSender` as a separate struct, not on Client trait.

**Location:** `cli/src/hub/commands.rs:312-399`

**Impact:** Clients aren't truly uniform - they need to know about HubCommandSender separately from the Client trait.

---

### 1.5 HIGH: State Duplication Between TuiRunner and TuiClient

**TuiRunner fields** (`cli/src/tui/runner.rs:108-138`):
```rust
selected_agent: Option<String>,
active_pty_view: PtyView,
pty_rx: Option<broadcast::Receiver<PtyEvent>>,
pty_handle: Option<PtyHandle>,
```

**TuiClient fields** (`cli/src/client/tui.rs:62-94`):
```rust
selected_agent: Option<String>,
active_pty_view: PtyView,
pty_event_rx: Option<broadcast::Receiver<PtyEvent>>,
current_pty_handle: Option<PtyHandle>,
```

**Issue:** IDENTICAL state exists in both. When they drift, bugs occur.

**Question:** Is TuiRunner supposed to own TuiClient and delegate? Or replace it? Migration appears incomplete.

---

### 1.6 HIGH: Naming Inconsistency

The codebase uses three names for the same concept:
- `agent_key` - TuiRunner, Hub commands
- `agent_id` - AgentInfo, Hub events, relay
- `session_key` - Agent struct, tunnel, server modules

**Locations:**
- `cli/src/agent/mod.rs:pub fn session_key(&self) -> String`
- `cli/src/hub/events.rs:agent_id: String`
- `cli/src/tui/runner.rs:selected_agent` (uses agent_key)

**Impact:** Confusion when debugging, potential for bugs when passing between layers.

---

### 1.7 MEDIUM: `.expect()` in Hot Paths

Parser lock expects without recovery in multiple hot paths:

**Locations:**
- `cli/src/tui/runner.rs:336, 500, 729, 824`
- `cli/src/tui/render.rs` (render loop)
- `cli/src/agent/scroll.rs` (6 locations)

**Pattern found:**
```rust
let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
```

**Issue:** If lock ever poisons (panic inside lock scope), entire TUI crashes with no recovery.

---

### 1.8 MEDIUM: Browser Output Hot Path Spawns Task Per Chunk

**Location:** `cli/src/client/browser.rs:311-312`
```rust
tokio::spawn(async move {
    if let Err(e) = sender.send(&json).await { ... }
});
```

**Issue:** At 60fps with full output, spawns ~6000 tokio tasks/second. Should batch.

---

### 1.9 LOW: on_owner_changed Signature Differs from Design

**Design doc:** `fn on_owner_changed(&mut self, new_owner: Option<ClientId>)`
**Actual:** `fn on_owner_changed(&mut self, is_owner: bool)`

**Assessment:** Intentional simplification (each client only cares if IT is owner). Document this deviation.

---

## 2. Hot Paths That Need Testing

### 2.1 PTY Output → TUI Display

```
Reader Thread (agent/spawn.rs:130)
    → broadcast::send(PtyEvent::Output)
    → TuiRunner.poll_pty_events() (tui/runner.rs:815-848)
    → vt100_parser.process(&data) (runner.rs:825)
    → ratatui render (runner.rs:920)
```

**Test cases:**
- [ ] Output reaches TUI within 16ms (one frame)
- [ ] Large output (>4KB) doesn't block
- [ ] 100 events processed per tick (batching works)
- [ ] Parser lock held briefly (<1ms)
- [ ] Lagged receiver recovers gracefully

---

### 2.2 PTY Output → Browser Display (BROKEN)

```
Reader Thread
    → broadcast::send(PtyEvent::Output)
    → Hub polls PTY events
    → client.on_output(&data) (browser.rs:303-318)
    → active_pty_sender.send() (FAILS - sender is None)
```

**Test cases:**
- [ ] BrowserClient receives hub_channel on connect
- [ ] BrowserClient receives pty_channels when agent selected
- [ ] Output actually reaches WebSocket
- [ ] Multiple browsers receive same output
- [ ] Slow browser doesn't block other browsers

---

### 2.3 TUI Input → PTY

```
crossterm::Event (tui/runner.rs:294-295)
    → process_event() (tui/input.rs)
    → InputResult::PtyInput(data)
    → handle_pty_input() (runner.rs:323-330)
    → pty_handle.write_input_blocking() (agent_handle.rs:249)
    → PtyCommand::Input sent via mpsc
    → PtySession.process_commands() (pty/mod.rs:247-295)
    → PtySession.write_input() (pty/mod.rs:510-516)
```

**Test cases:**
- [ ] Keystroke reaches PTY within 10ms
- [ ] Special keys (arrows, backspace) encode correctly
- [ ] Ctrl+C sends interrupt signal
- [ ] Paste (multi-byte) handles correctly
- [ ] No input lost under rapid typing

---

### 2.4 Browser Input → PTY

```
WebSocket → ActionCable
    → RelayConnection.recv()
    → BrowserCommand::Input parsed
    → BrowserEvent::Input created
    → browser_event_to_client_action() (relay/events.rs:43-150)
    → HubAction::SendInputForClient
    → Hub.dispatch_action()
    → PtySession.write_input()
```

**Test cases:**
- [ ] Browser keystroke reaches PTY
- [ ] Input routed to correct agent (per-browser selection)
- [ ] Multiple browsers can type to different agents
- [ ] Encrypted input decrypts correctly

---

### 2.5 Agent Creation Flow

```
TUI: Menu select "New Agent"
    → HubCommand::CreateAgent (tui/runner.rs:622)
    → Hub.process_commands() (hub/mod.rs:434-446)
    → spawn_cli_pty() (agent/pty/cli.rs:76-136)
    → Reader thread started (agent/spawn.rs:88-140)
    → HubEvent::AgentCreated broadcast
    → All clients receive on_agent_created()
```

**Test cases:**
- [ ] Agent appears in TUI list after creation
- [ ] Agent appears in browser list after creation
- [ ] PTY output starts flowing immediately
- [ ] Creation progress events fire in order
- [ ] Failed creation sends error event

---

### 2.6 Client Connect to PTY (Subscribe)

```
TUI: Select agent
    → HubCommand::GetAgent (tui/runner.rs:702)
    → Hub returns AgentHandle
    → TuiRunner.apply_agent_handle() (runner.rs:714-732)
    → pty_handle.subscribe() (agent_handle.rs:223-226)
    → broadcast::Receiver created
    → Parser reset for new agent (runner.rs:729-731)
```

**Test cases:**
- [ ] Subscription succeeds for CLI PTY
- [ ] Subscription succeeds for Server PTY
- [ ] Old subscription dropped on agent switch
- [ ] Parser cleared on agent switch
- [ ] Scrollback replayed on connect

---

### 2.7 Client Disconnect from PTY

```
Client disconnects (close browser, switch agent)
    → PtyHandle.disconnect() (agent_handle.rs:312-328)
    → PtyCommand::Disconnect sent
    → PtySession.disconnect() (pty/mod.rs:384-405)
    → Remove from connected_clients
    → If was owner: transfer ownership
    → Broadcast PtyEvent::OwnerChanged
```

**Test cases:**
- [ ] Client removed from connected_clients list
- [ ] Ownership transfers to next-newest client
- [ ] PTY resizes to new owner's dimensions
- [ ] OwnerChanged event fires
- [ ] No crash if last client disconnects

---

### 2.8 Terminal Resize Flow

```
TUI terminal resized
    → crossterm::Event::Resize
    → TuiRunner.handle_resize() (runner.rs:332-348)
    → Parser resize (runner.rs:337)
    → pty_handle.resize_blocking() (runner.rs:342-346)
    → PtyCommand::Resize sent
    → PtySession.client_resized() (pty/mod.rs:417-433)
    → If owner: PtySession.resize() (pty/mod.rs:484-503)
    → Broadcast PtyEvent::Resized
```

**Test cases:**
- [ ] Parser updates to new size
- [ ] PTY resizes if client is owner
- [ ] PTY doesn't resize if client is not owner
- [ ] Resize event broadcast to all clients
- [ ] Browser resize triggers same flow

---

### 2.9 Size Ownership Logic

```
Client A connects (becomes owner)
    → Client B connects (becomes owner, A loses)
    → Client B disconnects
    → Client A regains ownership
    → PTY resizes to A's dimensions
```

**Test cases:**
- [ ] Newest client becomes owner on connect
- [ ] Ownership transfers on disconnect
- [ ] Correct dimensions after ownership transfer
- [ ] on_owner_changed(true) sent to new owner
- [ ] on_owner_changed(false) sent to old owner

---

### 2.10 Hub Event Broadcasting

```
Hub state changes (agent created, deleted, status change)
    → hub.broadcast_event(HubEvent::*)
    → All subscribers receive via hub_event_rx
    → TuiClient.on_agent_created() / on_agent_deleted()
    → BrowserClient.on_agent_created() / on_agent_deleted()
```

**Test cases:**
- [ ] TUI receives AgentCreated event
- [ ] Browser receives AgentCreated event
- [ ] Multiple browsers all receive event
- [ ] Event contains correct AgentInfo
- [ ] Shutdown event received by all clients

---

## 3. Integration Test Scenarios

### 3.1 Full TUI Flow

```
1. Start Hub
2. Create agent via TUI menu
3. Verify agent appears in list
4. Select agent
5. Type input, verify echo
6. Verify output renders correctly
7. Resize terminal, verify PTY resizes
8. Delete agent
9. Verify removed from list
```

### 3.2 Full Browser Flow (Currently Broken)

```
1. Start Hub
2. Connect browser via QR code
3. Verify agent list received
4. Create agent via browser
5. Verify agent appears
6. Select agent
7. Verify terminal output streams
8. Type input, verify reaches PTY
9. Resize browser, verify PTY resizes
10. Disconnect browser
11. Verify cleanup (no memory leak)
```

### 3.3 Multi-Client Scenario

```
1. Start Hub
2. Connect TUI
3. Connect Browser A
4. Connect Browser B
5. Create agent
6. All three see agent in list
7. TUI selects agent (becomes size owner)
8. Browser A selects same agent
9. Browser A becomes size owner
10. TUI loses ownership, PTY keeps Browser A size
11. Browser A disconnects
12. TUI regains ownership, PTY resizes to TUI
```

### 3.4 Stress Test: Output Throughput

```
1. Create agent running `yes` or equivalent
2. Verify TUI renders at 60fps
3. Verify browser receives output
4. Verify no memory growth over 60 seconds
5. Verify CPU usage reasonable
```

### 3.5 Stress Test: Input Throughput

```
1. Create agent with cat
2. Paste 1MB of text
3. Verify all text reaches PTY
4. Verify echo received
5. Verify no input lost
```

---

## 4. Remediation Priority

### P0 - Critical (Must Fix Before Release)

1. **Wire BrowserClient channels** - browsers literally cannot receive output
   - `hub/actions.rs:handle_client_connected()` must wire channels
   - Create PTY channels when browser selects agent

2. **Move scroll.rs to client layer** - view state doesn't belong in Agent

3. **Move screen.rs to tui/relay** - rendering doesn't belong in Agent

### P1 - High (Fix Soon)

4. **Move menu.rs to tui** - TUI presentation doesn't belong in Hub

5. **Add Hub commands to Client trait** - for uniformity per design doc

6. **Resolve TuiRunner/TuiClient duplication** - pick one source of truth

7. **Unify naming: agent_key/agent_id/session_key** - pick one, use everywhere

### P2 - Medium (Fix When Possible)

8. **Replace .expect() with proper error handling** in hot paths

9. **Batch browser output sends** - don't spawn task per chunk

10. **Decouple Hub from TUI** - use dependency injection

### P3 - Low (Nice to Have)

11. **Document on_owner_changed signature change** from design doc

12. **Add newtype wrappers** for identifiers (compile-time safety)

---

## 5. Files to Modify

| File | Changes Needed |
|------|----------------|
| `agent/scroll.rs` | DELETE - move to `client/scroll.rs` |
| `agent/screen.rs` | DELETE - move to `tui/screen.rs` or `relay/screen.rs` |
| `hub/menu.rs` | DELETE - move to `tui/menu.rs` |
| `hub/actions.rs` | Fix `handle_client_connected()` to wire browser channels |
| `hub/run.rs` | Remove TUI imports, use dependency injection |
| `client/mod.rs` | Add `create_agent`, `delete_agent`, `list_agents` to trait |
| `client/tui.rs` | Implement new trait methods |
| `client/browser.rs` | Implement new trait methods |
| `tui/runner.rs` | Delegate to TuiClient OR remove TuiClient duplication |

---

## 6. Verification Checklist

After fixes are applied, verify:

- [ ] All tests in `cli/tests/` pass
- [ ] TUI can create, view, delete agents
- [ ] Browser can connect and see agent list
- [ ] Browser can view terminal output
- [ ] Browser input reaches PTY
- [ ] Multiple browsers work simultaneously
- [ ] Resize works for both TUI and browser
- [ ] Size ownership transfers correctly
- [ ] No memory leaks under load
- [ ] No panics in hot paths
