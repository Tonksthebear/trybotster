# Phase 5 Migration Plan: Complete the Client/PTY Refactor

**Status:** IN PROGRESS
**Created:** 2026-01-21
**Context:** CLIENT_REFACTOR_DESIGN.md spec is 70% complete. This document details remaining work.

---

## Overview

The codebase is in a **hybrid state** with both legacy and new architectures running in parallel.
This plan removes ALL legacy code and fully wires the new event-driven architecture.

**Key Principle:** TDD style - write/update tests FIRST, then implement, then verify.

---

## Pre-Migration Checklist

Before starting, verify current state:

```bash
# All tests should pass (1 flaky timing test is OK)
cd /Users/jasonconigliari/Rails/trybotster/cli && cargo test

# Rails tests should pass
cd /Users/jasonconigliari/Rails/trybotster && bin/rails test test/integration/cli_agent_lifecycle_test.rb test/system/terminal_relay_test.rb
```

---

## PHASE 5.1: Remove Legacy vt100_parser from PtySession

**Goal:** TuiClient owns its parser. PtySession just broadcasts raw bytes.

### 5.1.1 Update render.rs to use TuiClient's parser

**File:** `cli/src/render.rs`

**Current (WRONG):**
```rust
// Line 63-65
let parser = agent.get_active_parser();
let parser_lock = parser.lock().expect("parser lock not poisoned");
let screen = parser_lock.screen();
```

**Target:**
```rust
// Get TuiClient's parser from Hub's client registry
// The TuiClient already owns a vt100_parser - use that instead of agent's
```

**Steps:**
1. [ ] Add test in `cli/src/render.rs` tests that verifies render uses TuiClient parser
2. [ ] Modify `render_agent_terminal()` signature to take `&TuiClient` or `Arc<Mutex<Parser>>`
3. [ ] Update call site in `cli/src/tui/render.rs:212` to pass TuiClient's parser
4. [ ] Verify test passes

### 5.1.2 Remove vt100_parser from PtySession

**File:** `cli/src/agent/pty/mod.rs`

**Lines to remove:**
- Line 168-169: `pub vt100_parser: Arc<Mutex<Parser>>` field
- Line 221-222: Parser creation in `PtySession::new()`
- Line 231: `vt100_parser` field initialization
- Lines 499-506: `resize()` method's parser resize code
- Lines 572-576: `get_vt100_screen()` method
- Lines 583-587: `get_screen_as_ansi()` method
- Lines 592-595: `get_screen_hash()` method

**Steps:**
1. [ ] Find all usages of `pty.vt100_parser` or `agent.cli_pty.vt100_parser`
2. [ ] Update each usage to use TuiClient's parser instead
3. [ ] Remove the field and related methods
4. [ ] Run `cargo test` - fix any compilation errors
5. [ ] Run Rails tests to verify browser still works

### 5.1.3 Remove parser from spawn.rs reader thread

**File:** `cli/src/agent/spawn.rs`

**Current (lines 79-131):**
```rust
pub fn spawn_cli_reader_thread(
    reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<Parser>>,        // REMOVE THIS
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    raw_output_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,  // REMOVE THIS
    notification_tx: Sender<AgentNotification>,
    event_tx: broadcast::Sender<PtyEvent>,
) -> thread::JoinHandle<()> {
    // ...
    // Lines 127-131 process through parser - REMOVE
    {
        let mut p = parser.lock().expect("parser lock poisoned");
        p.process(&buf[..n]);
    }
```

**Target signature:**
```rust
pub fn spawn_cli_reader_thread(
    reader: Box<dyn Read + Send>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    notification_tx: Sender<AgentNotification>,
    event_tx: broadcast::Sender<PtyEvent>,
) -> thread::JoinHandle<()>
```

**Steps:**
1. [ ] Update function signature to remove `parser` and `raw_output_queue` params
2. [ ] Remove parser.process() call (lines 127-131)
3. [ ] Remove raw_output_queue push (lines 144-149)
4. [ ] Update call site in `cli/src/agent/pty/cli.rs:99-106`
5. [ ] Update call site in `cli/src/agent/pty/server.rs` (similar)
6. [ ] Run tests

### 5.1.4 Remove Agent.get_active_parser() method

**File:** `cli/src/agent/mod.rs`

**Steps:**
1. [ ] Find `get_active_parser` method
2. [ ] Find all call sites
3. [ ] Replace with TuiClient parser access
4. [ ] Remove the method
5. [ ] Run tests

---

## PHASE 5.2: Remove raw_output_queue from PtySession

**Goal:** Output flows ONLY through broadcast channel, not legacy queue.

### 5.2.1 Remove raw_output_queue field

**File:** `cli/src/agent/pty/mod.rs`

**Lines to remove:**
- Line 174-175: `pub raw_output_queue: Arc<Mutex<VecDeque<Vec<u8>>>>` field
- Line 233: Field initialization in `new()`
- Lines 463-471: `drain_raw_output()` method

**Steps:**
1. [ ] Find all usages of `raw_output_queue` or `drain_raw_output()`
2. [ ] Each usage should already have a broadcast equivalent - verify
3. [ ] Remove the field and method
4. [ ] Run tests

### 5.2.2 Update relay output routing

**File:** `cli/src/relay/mod.rs` (and related)

**Current:** Uses `drain_raw_output()` to get PTY output for browser streaming

**Target:** Subscribe to PtyEvent broadcast and forward to browser

**Steps:**
1. [ ] Find `drain_raw_output` calls in relay code
2. [ ] Replace with broadcast subscription pattern
3. [ ] Verify browser still receives output
4. [ ] Run Rails terminal_relay_test.rb

---

## PHASE 5.3: Remove Channel from PtySession (Move to BrowserClient)

**Goal:** BrowserClient manages its own encrypted channels.

### 5.3.1 Document current channel usage

**File:** `cli/src/agent/pty/mod.rs`

**Lines to analyze:**
- Line 181-184: `channel: Option<ActionCableChannel>` field
- Lines 405-438: `connect_channel()` method
- Lines 441-453: `has_channel()`, `get_channel_sender()` methods

**Steps:**
1. [ ] Trace all channel usages in relay code
2. [ ] Document which browser operations use agent.cli_pty.channel
3. [ ] Plan migration to BrowserClient-owned channels

### 5.3.2 Wire BrowserClient channel management

**File:** `cli/src/client/browser.rs`

**Current (lines 90-93 COMMENTED OUT):**
```rust
// === Channels (to be wired in Phase 5) ===
// hub_channel: Option<ActionCableChannel>,
// pty_channels: HashMap<PtyKey, ActionCableChannel>,
```

**Target:**
```rust
/// Hub channel for agent CRUD operations.
hub_channel: Option<ActionCableChannel>,
/// PTY channels for terminal I/O, keyed by (agent_id, pty_index).
pty_channels: HashMap<(String, usize), ActionCableChannel>,
```

**Steps:**
1. [ ] Uncomment and implement channel fields
2. [ ] Add `connect_hub_channel()` method
3. [ ] Add `connect_pty_channel(agent_id, pty_index)` method
4. [ ] Add `disconnect_pty_channel(agent_id, pty_index)` method
5. [ ] Wire on_output() to encrypt and send via pty_channel
6. [ ] Wire send_input() to receive from pty_channel and write to PTY
7. [ ] Write tests for channel lifecycle

### 5.3.3 Remove channel from PtySession

**Steps:**
1. [ ] Verify BrowserClient channels work
2. [ ] Remove `channel` field from PtySession
3. [ ] Remove `connect_channel()`, `has_channel()`, `get_channel_sender()` methods
4. [ ] Update all relay code to use BrowserClient channels
5. [ ] Run tests

---

## PHASE 5.4: Wire Client Input Routing

**Goal:** TuiClient.send_input() and BrowserClient.send_input() actually work.

### 5.4.1 Wire TuiClient.send_input()

**File:** `cli/src/client/tui.rs`

**Current (lines 218-223):**
```rust
fn send_input(&mut self, _data: &[u8]) -> Result<(), String> {
    // TUI sends input directly to PTY via write_input()
    // This is wired up in the event loop, not here
    // For now, return error - will be properly wired in Phase 5
    Err("TUI input routing not yet wired".to_string())
}
```

**Options:**
1. TuiClient holds reference to current PtySession's writer
2. TuiClient sends via Hub command channel
3. Keep current pattern where event loop handles input directly

**Chosen approach:** Option 3 is already working for TUI. The trait method exists for uniformity but TUI input goes directly through event loop → Hub → PTY. Mark this as intentional, not an error.

**Steps:**
1. [ ] Change return to `Ok(())` with comment explaining TUI input flow
2. [ ] Or: Add `pty_writer: Option<...>` field and wire it on agent select
3. [ ] Write test that TUI input reaches PTY

### 5.4.2 Wire BrowserClient.send_input()

**File:** `cli/src/client/browser.rs`

**Current (lines 244-248):**
```rust
fn send_input(&mut self, _data: &[u8]) -> Result<(), String> {
    // TODO (Phase 5): Write input to connected PTY
    Err("Browser input routing not yet wired".to_string())
}
```

**Target:**
```rust
fn send_input(&mut self, data: &[u8]) -> Result<(), String> {
    // Browser input comes in via pty_channel, is decrypted by relay,
    // and this method writes to the PTY.
    // Get the writer for the connected agent's PTY and write.
    if let Some(writer) = &mut self.pty_writer {
        writer.write_all(data).map_err(|e| e.to_string())?;
        Ok(())
    } else {
        Err("Not connected to any PTY".to_string())
    }
}
```

**Steps:**
1. [ ] Add `pty_writer` field to BrowserClient
2. [ ] Set writer when connecting to PTY
3. [ ] Implement send_input() to write to PTY
4. [ ] Write test for browser input flow

---

## PHASE 5.5: Wire Hub Command Channel for Agent CRUD

**Goal:** CreateAgent and DeleteAgent work via command channel.

### 5.5.1 Implement CreateAgent via command channel

**File:** `cli/src/hub/mod.rs`

**Current (lines 1593-1598):**
```rust
HubCommand::CreateAgent { request, response_tx } => {
    log::info!("Processing CreateAgent command: {:?}", request.issue_or_branch);
    // Delegate to existing action handling
    // For now, send error - full implementation in Phase 5
    let _ = response_tx.send(Err("CreateAgent via command channel not yet implemented".to_string()));
}
```

**Target:** Delegate to existing spawn_agent logic and send result.

**Steps:**
1. [ ] Write test that CreateAgent command creates an agent
2. [ ] Call existing `spawn_agent()` or equivalent
3. [ ] Build AgentInfo from result
4. [ ] Send via response_tx
5. [ ] Broadcast HubEvent::AgentCreated
6. [ ] Verify test passes

### 5.5.2 Implement DeleteAgent via command channel

**File:** `cli/src/hub/mod.rs`

**Current (lines 1599-1604):**
```rust
HubCommand::DeleteAgent { request, response_tx } => {
    log::info!("Processing DeleteAgent command: {:?}", request.agent_id);
    // For now, send error - full implementation in Phase 5
    let _ = response_tx.send(Err("DeleteAgent via command channel not yet implemented".to_string()));
}
```

**Steps:**
1. [ ] Write test that DeleteAgent command deletes an agent
2. [ ] Call existing `close_agent()` or equivalent
3. [ ] Handle worktree deletion if requested
4. [ ] Send Ok(()) via response_tx
5. [ ] Broadcast HubEvent::AgentDeleted
6. [ ] Verify test passes

---

## PHASE 5.6: Wire BrowserClient Hub Event Handlers

**Goal:** Browser receives agent lifecycle events.

### 5.6.1 Wire on_agent_created()

**File:** `cli/src/client/browser.rs`

**Current (lines 208-215):**
```rust
fn on_agent_created(&mut self, agent_id: &str, _info: &AgentInfo) {
    log::debug!("Browser {}: Agent created: {}", ...);
    // TODO (Phase 5): Send agent list update via hub_channel
}
```

**Steps:**
1. [ ] Encrypt agent info
2. [ ] Send via hub_channel as "agent_created" message
3. [ ] Write test verifying browser receives event

### 5.6.2 Wire on_agent_deleted()

**File:** `cli/src/client/browser.rs` lines 217-229

**Steps:**
1. [ ] Encrypt agent_id
2. [ ] Send via hub_channel as "agent_deleted" message
3. [ ] Write test

### 5.6.3 Wire on_hub_shutdown()

**File:** `cli/src/client/browser.rs` lines 231-238

**Steps:**
1. [ ] Send shutdown notification via hub_channel
2. [ ] Disconnect all channels gracefully
3. [ ] Write test

### 5.6.4 Wire on_process_exit()

**File:** `cli/src/client/browser.rs` lines 185-192

**Steps:**
1. [ ] Send process exit notification via pty_channel
2. [ ] Write test

### 5.6.5 Wire on_output()

**File:** `cli/src/client/browser.rs` lines 175-178

**Current:**
```rust
fn on_output(&mut self, _data: &[u8]) {
    // TODO (Phase 5): Encrypt data and send via pty_channel
}
```

**Steps:**
1. [ ] Get pty_channel for current agent
2. [ ] Encrypt data
3. [ ] Send via channel
4. [ ] Write test verifying output reaches browser

---

## PHASE 5.7: Remove Legacy Client Trait Methods

**Goal:** Clean Client trait with no deprecated methods.

### 5.7.1 Remove deprecated methods from trait

**File:** `cli/src/client/mod.rs`

**Lines to remove (227-307):**
```rust
// Legacy Support (DEPRECATED - to be removed in Phase 5)
fn state(&self) -> &ClientState;
fn state_mut(&mut self) -> &mut ClientState;
fn receive_output(&mut self, data: &[u8]) { ... }
fn receive_scrollback(&mut self, _lines: Vec<String>) { ... }
fn receive_agent_list(&mut self, _agents: Vec<AgentInfo>) { ... }
fn receive_worktree_list(&mut self, _worktrees: Vec<WorktreeInfo>) { ... }
fn receive_response(&mut self, _response: Response) { ... }
fn select_agent(&mut self, agent_key: &str) { ... }
fn clear_selection(&mut self) { ... }
fn resize(&mut self, cols: u16, rows: u16) { ... }
fn flush(&mut self) { ... }
fn drain_buffered_output(&mut self) -> Option<Vec<u8>> { ... }
```

**Steps:**
1. [ ] Find all call sites of each deprecated method
2. [ ] Replace with new architecture equivalent:
   - `state()` → use specific fields (dims, selected_agent)
   - `receive_output()` → `on_output()`
   - `select_agent()` → `set_connected_agent()` + PtySession.connect()
   - `resize()` → `update_dims()` + PtySession.client_resized()
3. [ ] Remove methods from trait
4. [ ] Remove from TuiClient implementation
5. [ ] Remove from BrowserClient implementation
6. [ ] Run tests

### 5.7.2 Remove ClientState struct

**File:** `cli/src/client/mod.rs`

**Lines 95-111:**
```rust
/// Per-client view state (LEGACY - to be removed in Phase 5).
#[derive(Debug, Clone, Default)]
pub struct ClientState {
    pub selected_agent: Option<String>,
    pub dims: Option<(u16, u16)>,
}
```

**Steps:**
1. [ ] Find all usages of ClientState
2. [ ] Replace with direct field access on client
3. [ ] Remove struct definition
4. [ ] Remove `state` field from TuiClient and BrowserClient
5. [ ] Run tests

---

## PHASE 5.8: Final Cleanup

### 5.8.1 Remove all DEPRECATED comments

**Steps:**
1. [ ] `grep -r "DEPRECATED" cli/src/` should return 0 results
2. [ ] `grep -r "Phase 5" cli/src/` should return 0 results
3. [ ] `grep -r "to be removed" cli/src/` should return 0 results

### 5.8.2 Remove LegacyClient reference from mod.rs

**File:** `cli/src/client/mod.rs` line 31

```rust
//! trait (`LegacyClient`). During Phase 5, `LegacyClient` will be removed.
```

**Steps:**
1. [ ] Remove this comment
2. [ ] Update module documentation to reflect new architecture

### 5.8.3 Update CLIENT_REFACTOR_DESIGN.md

**Steps:**
1. [ ] Mark all Phase 5 checklist items as complete
2. [ ] Update status from "Draft" to "Implemented"
3. [ ] Add implementation notes

### 5.8.4 Run full test suite

```bash
# Rust tests
cd /Users/jasonconigliari/Rails/trybotster/cli
cargo test
cargo clippy

# Rails integration tests
cd /Users/jasonconigliari/Rails/trybotster
bin/rails test test/integration/cli_agent_lifecycle_test.rb
bin/rails test test/system/terminal_relay_test.rb
bin/rails test test/system/agent_url_navigation_test.rb
```

### 5.8.5 Verify no dead code

```bash
# Check for unused code
cargo +nightly udeps  # if available
# Or manually review warnings from:
cargo build 2>&1 | grep "warning: unused"
```

---

## File Reference: All Files Requiring Changes

| File | Changes |
|------|---------|
| `cli/src/agent/pty/mod.rs` | Remove vt100_parser, raw_output_queue, channel fields and methods |
| `cli/src/agent/pty/cli.rs` | Update spawn call to remove legacy params |
| `cli/src/agent/pty/server.rs` | Update spawn call to remove legacy params |
| `cli/src/agent/spawn.rs` | Remove parser and raw_output_queue from reader thread |
| `cli/src/agent/mod.rs` | Remove get_active_parser() method |
| `cli/src/render.rs` | Use TuiClient parser instead of agent parser |
| `cli/src/tui/render.rs` | Pass TuiClient parser to render_agent_terminal |
| `cli/src/client/mod.rs` | Remove ClientState, remove deprecated trait methods |
| `cli/src/client/tui.rs` | Remove state field, implement send_input properly |
| `cli/src/client/browser.rs` | Add channels, wire all on_* methods, implement send_input |
| `cli/src/hub/mod.rs` | Implement CreateAgent/DeleteAgent commands |
| `cli/src/relay/*.rs` | Use BrowserClient channels instead of PtySession.channel |

---

## Verification Checklist

After completing all phases:

- [ ] `grep -r "DEPRECATED" cli/src/` returns nothing
- [ ] `grep -r "TODO.*Phase 5" cli/src/` returns nothing
- [ ] `grep -r "to be removed" cli/src/` returns nothing
- [ ] `grep -r "raw_output_queue" cli/src/` returns nothing
- [ ] `grep -r "LegacyClient" cli/src/` returns nothing
- [ ] `cargo test` passes (569+ tests)
- [ ] `cargo clippy` has no warnings
- [ ] Rails CLI integration tests pass (5 tests)
- [ ] Rails terminal relay tests pass (22 tests)
- [ ] Browser can connect and see terminal output
- [ ] Browser input reaches PTY
- [ ] TUI renders correctly
- [ ] Agent creation works from TUI menu
- [ ] Agent deletion works
- [ ] Multiple browsers can view same agent

---

## Order of Operations

Execute phases in this order to minimize breakage:

1. **5.4** - Wire TuiClient.send_input (quick win, low risk)
2. **5.5** - Wire Hub commands (enables testing)
3. **5.1** - Remove PtySession.vt100_parser (big change, do carefully)
4. **5.2** - Remove raw_output_queue (depends on 5.1)
5. **5.3** - Move channels to BrowserClient (complex, do last)
6. **5.6** - Wire BrowserClient event handlers (after channels work)
7. **5.7** - Remove legacy trait methods (cleanup)
8. **5.8** - Final cleanup and verification

---

## Recovery Plan

If something breaks badly:

1. Tests are the source of truth - if they pass, code is correct
2. Rails tests verify end-to-end browser functionality
3. Git history has all previous working states
4. The hybrid state (current) is functional - can always revert to it

---

## Notes for Future Context

- The codebase works TODAY with legacy + new running in parallel
- Don't remove legacy code until new code is verified working
- Browser functionality is the most sensitive - test thoroughly
- TUI is simpler - fewer moving parts
- PtySession.channel removal is the riskiest change
