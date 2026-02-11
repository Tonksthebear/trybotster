# Hot Path Audit: Browser ↔ CLI Data Flow

## Root Cause of Current Bug

**Race condition**: Browser sends messages asynchronously without ordering guarantees.

```
Browser sends:  set_mode → resize → create_agent  (intended order)
CLI receives:   create_agent → set_mode → resize  (possible actual order)
```

**Result**: Agent spawns with default dims (24×80) because resize hasn't arrived yet.

---

## Complete Hot Path Inventory

### INBOUND: Browser → CLI

| # | Message Type | Handler | Dims Used | Test Coverage |
|---|--------------|---------|-----------|---------------|
| 1 | `handshake` | connection.rs:367 | N/A | ❌ None |
| 2 | `set_mode` | browser.rs:121 | N/A | ❌ None |
| 3 | `resize` | `ResizeForClient` action | Updates client dims | ⚠️ Partial |
| 4 | `input` | `SendInputForClient` action | N/A | ⚠️ Partial |
| 5 | `select_agent` | `SelectAgentForClient` action | Applies client dims on select | ✅ Fixed |
| 6 | `create_agent` | browser.rs:126 → SpawnAgent | `hub.browser.dims` fallback to `terminal_dims` | ❌ **BUG** |
| 7 | `reopen_worktree` | browser.rs:131 → SpawnAgent | Same as above | ❌ **BUG** |
| 8 | `delete_agent` | `DeleteAgentForClient` action | N/A | ❌ None |
| 9 | `list_agents` | `RequestAgentList` action | N/A | ❌ None |
| 10 | `list_worktrees` | `RequestWorktreeList` action | N/A | ❌ None |
| 11 | `scroll` | browser.rs side effect | N/A | ❌ None |
| 12 | `toggle_pty_view` | browser.rs side effect | N/A | ❌ None |
| 13 | `generate_invite` | connection.rs:423 | N/A | ❌ None |

### OUTBOUND: CLI → Browser

| # | Message Type | Sender | Test Coverage |
|---|--------------|--------|---------------|
| 1 | `handshake_ack` | connection.rs:398 | ❌ None |
| 2 | `agent_list` | `send_agent_list()` | ❌ None |
| 3 | `worktree_list` | `send_worktree_list()` | ❌ None |
| 4 | `agent_selected` | `send_agent_selected()` | ❌ None |
| 5 | `scrollback` | `send_scrollback()` | ❌ None |
| 6 | `output` (PTY) | `drain_and_route_pty_output()` | ❌ None |
| 7 | `invite_bundle` | connection.rs:469 | ❌ None |
| 8 | `error` | various | ❌ None |

---

## Critical Bugs Identified

### BUG 1: CreateAgent uses wrong dims source (CRITICAL)

**Location**: `browser.rs:260` → `actions.rs:423-427`

**Flow**:
```
BrowserEvent::CreateAgent
  → browser::create_agent()
  → dispatch(SpawnAgent)
  → dims = hub.browser.dims.map_or(hub.terminal_dims, ...)
  → lifecycle::spawn_agent(dims)
```

**Problem**: Uses `hub.browser.dims` which may be None if resize hasn't arrived yet.

**Fix needed**: CreateAgent should use `hub.clients.get(&browser_identity).dims` like `CreateAgentForClient` does.

### BUG 2: ReopenWorktree has same issue

**Location**: `browser.rs:310` → `actions.rs:423-427`

Same fix needed.

### BUG 3: Legacy path doesn't convert to client-scoped action

**Location**: `browser.rs:126-133`

`CreateAgent` and `ReopenWorktree` are NOT converted to client-scoped actions via `browser_event_to_client_action()`. They're handled in the legacy fallback path.

**Fix needed**: Convert to `CreateAgentForClient` and `ReopenWorktreeForClient` actions.

### BUG 4: No message ordering guarantee

**Browser JS** (`terminal_display_controller.js:113-114`):
```javascript
this.connection.send("set_mode", { mode: "gui" });  // Fire & forget
this.sendResize();                                    // Fire & forget
```

**Fix needed**: Browser should `await` critical messages, or CLI should buffer create_agent until resize received.

---

## Spawn Dimension Sources (3 paths)

| Path | Code Location | Dims Source | Correct? |
|------|---------------|-------------|----------|
| `SpawnAgent` action | actions.rs:423-427 | `hub.browser.dims` | ❌ Wrong |
| `spawn_agent_with_tunnel` | actions.rs:795-797 | `hub.browser.dims` | ❌ Wrong |
| `CreateAgentForClient` | actions.rs:959-962 | `client.state().dims` | ✅ Correct |

---

## Test Coverage Gaps

### Unit Tests Needed

1. **CreateAgent with no prior resize** - Should use reasonable default or fail gracefully
2. **CreateAgent after resize** - Should use resize dims
3. **ReopenWorktree with no prior resize** - Same
4. **Multiple browser clients with different dims** - Each gets correct size
5. **PTY output routing** - Output goes only to viewers
6. **Agent list broadcast** - All clients receive updates
7. **Scrollback on select** - Browser receives scrollback

### Integration Tests Needed (Rails system tests)

1. **Connect → Resize → CreateAgent** - Verify PTY has correct size
2. **Connect → CreateAgent → Resize** - Verify PTY gets resized after
3. **Two browsers, different selections** - Each sees correct agent
4. **Browser input → Agent PTY** - Full round trip
5. **Agent PTY output → Browser** - Full round trip

---

## Recommended Fixes (Priority Order)

### P0: Fix CreateAgent dims (root cause of screenshot bug)

Option A: Convert CreateAgent to client-scoped action
```rust
// In events.rs browser_event_to_client_action()
BrowserEvent::CreateAgent { issue_or_branch, prompt } => {
    Some(HubAction::CreateAgentForClient {
        client_id,
        request: CreateAgentRequest { issue_or_branch, prompt, from_worktree: None },
    })
}
```

Option B: Fix SpawnAgent to use browser identity dims
```rust
// In actions.rs SpawnAgent handler
let dims = if let Some(browser_dims) = &hub.browser.dims {
    (browser_dims.rows, browser_dims.cols)
} else {
    hub.terminal_dims
};
```

### P1: Browser should await resize before enabling UI

```javascript
// In terminal_display_controller.js handleConnected()
async handleConnected(outlet) {
    this.connection = outlet;
    await this.connection.send("set_mode", { mode: "gui" });
    await this.sendResize();
    // Now UI can be enabled
}
```

### P2: CLI should validate dims before spawn

```rust
// In lifecycle::spawn_agent()
if dims.0 < 10 || dims.1 < 20 {
    log::warn!("Spawning agent with tiny dims {:?}, using default", dims);
    dims = (24, 80);
}
```

---

## Message Sequence Diagrams

### Current (Broken) Flow

```
Browser                     CLI
   |                         |
   |--[connected]----------->| (no dims yet)
   |--[set_mode]------------>| hub.browser.mode = gui
   |--[create_agent]-------->| spawns with (24,80) ← BUG!
   |--[resize 100x50]------->| sets hub.browser.dims
   |                         | (too late!)
```

### Expected (Fixed) Flow

```
Browser                     CLI
   |                         |
   |--[connected]----------->|
   |--[set_mode]------------>|
   |--[resize 100x50]------->| client.dims = (100,50)
   |                         |
   |--[create_agent]-------->| spawns with client.dims (100,50) ✓
```

---

## Additional Bugs Found (2026-01-12)

### BUG 5: Scrollback Broadcasts to ALL Browsers (CRITICAL)

**Location:** `relay/browser.rs:95-99` (SelectAgent handler)

**Issue:** When Browser A selects agent-1, the scrollback history is sent to ALL connected browsers, not just Browser A.

**Code path:**
```
BrowserEvent::SelectAgent → send_scrollback_for_agent(hub, id) →
  relay::send_scrollback(&ctx, lines) → sender.send() → BROADCAST
```

**Expected:** Only the selecting browser receives scrollback.

**Fix:** Pass browser identity, use `sender.send_to(identity, ...)`.

---

### BUG 6: Agent Selected Notification Broadcasts to ALL Browsers

**Location:** `relay/browser.rs:97` (SelectAgent handler)

**Issue:** When Browser A selects agent-1, ALL browsers receive a "agent_selected" notification, not just Browser A.

**Code path:**
```
BrowserEvent::SelectAgent → send_agent_selected(hub, agent_id) →
  relay::send_agent_selected(&ctx, agent_id) → sender.send() → BROADCAST
```

**Expected:** Only the selecting browser receives confirmation.

---

### BUG 7: Initial Connect Data Broadcasts to ALL Browsers

**Location:** `relay/browser.rs:84-86` (Connected handler)

**Issue:** When Browser A connects, initial agent list and worktree list are sent to ALL connected browsers.

**Impact:** Lower severity - duplicates data to existing browsers.

---

## Missing Tests Identified

### Per-Client Routing Tests

```rust
// Test: Scrollback should go to selecting browser only
#[test]
fn test_scrollback_sent_to_selecting_browser_only() {
    // Browser A and B both connected
    // Browser A selects agent-1
    // ASSERT: Only Browser A receives scrollback
    // ASSERT: Browser B does NOT receive scrollback
}

// Test: Agent selected notification should go to selecting browser only
#[test]
fn test_agent_selected_sent_to_selecting_browser_only() {
    // Browser A selects agent-1
    // ASSERT: Only Browser A receives AgentSelected
    // ASSERT: Browser B does NOT receive it
}

// Test: Initial data should go to new browser only
#[test]
fn test_initial_data_sent_to_new_browser_only() {
    // Browser A already connected
    // Browser B connects
    // ASSERT: Agent list only goes to Browser B
    // ASSERT: Browser A does NOT receive duplicate
}

// Test: Browser disconnect cleans up viewer index
#[test]
fn test_disconnect_cleans_viewer_index() {
    // Browser viewing agent
    // Browser disconnects
    // ASSERT: Viewer index empty for that agent
    // ASSERT: Subsequent output doesn't try to route to disconnected browser
}
```

---

## Fix Strategy for Broadcast Bugs

The root cause is that `relay/state.rs` send functions use `sender.send()` (broadcast) instead of `sender.send_to(identity, ...)` (targeted).

### Approach: Add identity parameter to browser.rs handlers

```rust
// In browser.rs poll_events_headless
BrowserEvent::SelectAgent { id } => {
    hub.browser.invalidate_screen();
    send_agent_selected_to(hub, &browser_identity, id);  // Targeted
    send_scrollback_for_agent_to(hub, &browser_identity, id);  // Targeted
}

BrowserEvent::Connected { device_name, .. } => {
    hub.browser.handle_connected(device_name);
    send_agent_list_to(hub, &browser_identity);  // Targeted
    send_worktree_list_to(hub, &browser_identity);  // Targeted
}
```

### New relay/state.rs functions needed:

```rust
pub fn send_agent_selected_to(ctx: &BrowserSendContext, identity: &str, agent_id: &str) { ... }
pub fn send_scrollback_to(ctx: &BrowserSendContext, identity: &str, lines: Vec<String>) { ... }
pub fn send_agent_list_to(ctx: &BrowserSendContext, identity: &str, agents: Vec<AgentInfo>) { ... }
pub fn send_worktree_list_to(ctx: &BrowserSendContext, identity: &str, worktrees: Vec<WorktreeInfo>) { ... }
```

---

## Fixes Applied (2026-01-12)

### Fix 1: Targeted send functions added to relay/state.rs ✓
- `send_agent_list_to()`, `send_agent_selected_to()`, `send_scrollback_to()`, `send_worktree_list_to()`

### Fix 2: browser.rs side effects updated ✓
- Connected: Uses targeted sends for agent list and worktree list
- SelectAgent: Uses targeted sends for agent_selected and scrollback
- **NEW**: CreateAgent/ReopenWorktree: Sends agent list (broadcast) + agent_selected and scrollback to creating browser

### Fix 3: RequestAgentList/RequestWorktreeList now use targeted sends ✓
- Previously used broadcast `send_agent_list(hub)` for all browsers
- Now uses `send_agent_list_to_browser(hub, identity)` for requesting browser only

### Fix 4: ClientId.browser_identity() helper added ✓
- Extracts identity string from `ClientId::Browser(identity)`

### Test Count
- Before: 373 tests
- After: 376 tests (added 3 new tests documenting expected behavior)
