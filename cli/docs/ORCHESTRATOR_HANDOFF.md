# Orchestrator Handoff Document

**Date:** 2026-01-22
**Previous Session:** TUI test rewrite + PTY architecture debugging

---

## Executive Summary

A massive refactor is in progress. The implementation has drifted significantly from the spec (`cli/docs/CLIENT_REFACTOR_DESIGN.md`). Multiple agents made changes that added complexity instead of following the documented architecture. The core issue: **the spec defines a simple direct-call architecture, but the implementation uses command channels and Hub routing.**

---

## What Was Accomplished This Session

### Completed Work
1. **TUI tests rewritten** - 47 end-to-end tests replacing fake stub tests
2. **7 deadlocked hub tests fixed** - Added background thread pattern for command processing
3. **Menu navigation bug fixed** - Changed from hardcoded `MENU_ITEMS.len()` to dynamic `selectable_count()`
4. **Agent list not updating fixed** - Added `HubEvent::agent_created()` broadcast in `spawn_agent_sync()`
5. **PTY command processor added** - PtySession now spawns its own tokio task to process commands

### What's Still Broken
1. **TUI display is garbled** - Dimension mismatch between PTY and parser
2. **Input may not work** - Architecture confusion around how input flows
3. **Client trait parity is broken** - TUI and Browser have fundamentally different implementations

---

## The Core Problem: Spec vs Implementation Drift

### What The Spec Says (CLIENT_REFACTOR_DESIGN.md)

**Section 5.3 - Client Input Flow:**
```
Client → pty.write_input(data) → PTY stdin
```
Direct call. No Hub. No command channels.

**Section 3.3 - PtySession Exposes:**
- `connect(client_id, dims)` → returns subscription receiver
- `disconnect(client_id)`
- `client_resized(client_id, dims)`
- `write_input(data)` ← **DIRECT METHOD CALL**

**Section 3.4 - Client Trait:**
```rust
pub trait Client: Send {
    fn send_input(&mut self, data: &[u8]);  // Calls pty.write_input() directly
    // ...
}
```

**Section 2.4 - Key Design Decision:**
> "All clients run in their own threads and use Hub command channels."
> BUT this is for **Hub commands** (create/delete agent), NOT for PTY I/O.

**Section 3.2 - Hub Does NOT:**
- Track which client is viewing which PTY (that's PtySession's job)
- Route PTY output (that's pub/sub)
- Manage resize logic (that's PtySession's job)

### What The Implementation Does (WRONG)

1. **PtyHandle with command channel** - Instead of direct `write_input()`, we have:
   - `PtyHandle.write_input_blocking()` → sends `PtyCommand::Input` to channel
   - Channel sits in queue
   - Spawned task processes channel (we just added this)

2. **Hub routes browser input** - `HubAction::SendInputForClient` goes through Hub to Agent to PTY

3. **TuiClient holds PtyHandle** - Instead of holding PtySession reference

4. **BrowserClient.send_input() returns error** - Completely broken

---

## What The Spec Gets Wrong (Or Is Ambiguous)

1. **How client gets PtySession reference** - Spec shows `pty.connect()` but doesn't clarify how client obtains `pty` in the first place. Hub owns Agents which own PtySessions.

2. **Multiple PTY connections** - Section 2.4 says "architecture explicitly supports a single client connecting to multiple PtySessions simultaneously" but doesn't detail how client manages multiple references.

3. **Thread ownership** - If TuiClient runs in its own thread and PtySession is owned by Agent (owned by Hub), how does TuiClient get a reference? Arc<Mutex<>>? The spec doesn't say.

---

## Architecture Decisions Needed

### Option A: Follow Spec Literally
- Client holds `Arc<PtySession>` or similar
- `Client.send_input()` calls `pty.write_input()` directly
- No command channels for PTY I/O
- Simpler, matches spec diagrams

### Option B: Keep Command Channel Pattern
- PtyHandle is the interface (current)
- Command channel + processor task (just implemented)
- More indirection but thread-safe by design
- Diverges from spec

### Recommendation: Option A
The spec was designed thoughtfully. The command channel pattern was likely added by agents who didn't read the spec. Return to the spec's simplicity.

---

## Files To Read First

1. **`cli/docs/CLIENT_REFACTOR_DESIGN.md`** - THE SPEC. Read it completely before making changes.
2. **`cli/docs/PHASE5_MIGRATION_PLAN.md`** - Migration steps
3. **`cli/docs/REFACTOR_VERIFICATION_SPEC.md`** - Success criteria

---

## Current Code State

### Key Files
- `cli/src/client/mod.rs` - Client trait (incomplete)
- `cli/src/client/tui.rs` - TuiClient (holds PtyHandle, has many non-trait methods)
- `cli/src/client/browser.rs` - BrowserClient (send_input returns error)
- `cli/src/agent/pty/mod.rs` - PtySession (has command channel + processor task)
- `cli/src/hub/agent_handle.rs` - PtyHandle (sends commands through channel)
- `cli/src/hub/actions.rs` - 14 places that downcast to TuiClient directly

### Test Status
- 685 tests passing
- 1 ignored (keyring access)
- Tests complete in ~4 seconds

---

## Specific Issues To Fix

### 1. Client Trait Is Incomplete
**Current trait missing:**
- `select_agent()` / `selected_agent()`
- `connect_to_pty()` / `disconnect_from_pty()`
- `scroll()` methods
- `toggle_view()` methods

**Hub code bypasses trait** in 14 places by downcasting to TuiClient.

### 2. Input Path Is Wrong
**Should be:** `Client.send_input()` → `PtySession.write_input()` (direct)
**Currently:** `TuiClient` → `PtyHandle` → command channel → processor task → write

### 3. Browser Input Path Is Different
**TUI:** Uses PtyHandle
**Browser:** Hub routes via `SendInputForClient` action

Both should use the same path through the Client trait.

### 4. Dimension Mismatch Causes Garbled Display
- PTY created with hardcoded 24x80 (`cli/src/agent/mod.rs:160`)
- TUI has different dimensions
- Resize commands may not be processed correctly
- Parser dimensions may not match PTY dimensions

### 5. Multiple PTY Connections Not Implemented
Spec says clients can connect to multiple PtySessions. Current implementation assumes one active PTY per client.

---

## Suggested Approach

### Step 1: Re-read the Spec
Read `CLIENT_REFACTOR_DESIGN.md` completely. Understand the intended architecture before changing code.

### Step 2: Simplify PtySession
Remove command channel complexity. Expose direct methods:
- `connect(client_id, dims) -> Receiver<PtyEvent>`
- `disconnect(client_id)`
- `write_input(data)` ← direct write, no channel
- `client_resized(client_id, dims)`

### Step 3: Fix Client Trait
Add all missing methods. Both TUI and Browser should implement the complete trait.

### Step 4: Remove Hub From PTY Path
Hub should only:
- Create/delete agents
- List agents
- Get agent by ID

Hub should NOT:
- Route input
- Handle resize
- Track client→PTY mappings

### Step 5: Update TuiClient and BrowserClient
Both should:
- Hold reference(s) to connected PtySession(s)
- Implement `send_input()` by calling `pty.write_input()` directly
- Manage their own PTY subscriptions

### Step 6: Remove Downcasts
All 14 places in `hub/actions.rs` that downcast to TuiClient should use trait methods instead.

---

## Don't Repeat These Mistakes

1. **Don't add complexity without reading the spec** - The command channel pattern wasn't in the spec
2. **Don't let agents work without spec context** - They'll invent their own architecture
3. **Don't fix symptoms without understanding root cause** - We added a command processor when we should have questioned why commands exist
4. **Don't assume the current code is correct** - Multiple agents have modified it inconsistently

---

## Questions For User

Before proceeding, clarify with user:

1. **Should we follow the spec literally?** Or is the command channel pattern acceptable?
2. **How should clients get PtySession references?** Arc<Mutex<>>? AgentHandle returning reference?
3. **Is the spec's multiple-PTY-per-client feature needed now?** Or can we simplify to one active PTY?

---

## Test Commands

```bash
# Run all tests
cargo test --lib

# Run TUI tests specifically
cargo test --lib tui::runner

# Run hub tests
cargo test --lib hub::tests

# Build
cargo build
```

---

## Summary For Next Orchestrator

**You are orchestrating a refactor to align implementation with spec.**

1. Read `cli/docs/CLIENT_REFACTOR_DESIGN.md` first
2. The spec says: direct calls, no command channels for PTY I/O
3. Current code has: command channels, Hub routing, broken parity
4. Goal: Client trait provides ALL interactions, both clients wire to same PTY interface
5. Don't add complexity - remove it to match spec's simplicity
