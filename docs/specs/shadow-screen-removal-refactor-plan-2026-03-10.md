# Shadow Screen Removal Refactor Plan

**Date**: 2026-03-10
**Status**: Draft
**Author**: Refactoring analysis

## Executive Summary

Remove the hub-side AlacrittyParser (shadow screen) from broker-backed sessions, making the hub a dumb byte relay rather than a parallel terminal emulator. This eliminates ~5000-line scrollback buffers duplicated per session between hub and broker, removes the ghost session machinery, and simplifies the reconnect/subscribe path to use the broker's authoritative `GetSnapshot` RPC.

The refactoring is non-trivial because the hub's shadow screen currently serves **four distinct purposes** beyond snapshot generation:

1. **Snapshot on subscribe** -- `snapshot_and_subscribe_cached()` generates ANSI snapshots for client attach
2. **Dimensions tracking** -- `dims()` reads terminal size from the shadow screen grid
3. **Cursor visibility** -- `cursor_visible()` reads DECTCEM state from the shadow screen (used by Lua and MCP)
4. **Screen content extraction** -- `get_screen()` returns plain-text visible content (used by MCP tools)

Each of these must be replaced before the shadow screen can be removed.

## Current State Analysis

### Data Flow (Current)

```
Broker PTY reader thread
    |
    v
BrokerPtyOutput frame (raw bytes)
    |
    v
Hub event loop (server_comms.rs:893)
    |
    v
PtyHandle::feed_broker_output()
    |
    +---> process_pty_bytes()
    |         |
    |         +---> shadow_screen.lock().process(data)  <-- DUPLICATE PARSING
    |         +---> detect OSC notifications
    |         +---> detect kitty keyboard transitions
    |         +---> detect cursor visibility transitions
    |         +---> event_tx.send(PtyEvent::Output(data))
    |
    v
Clients (TUI panel, WebRTC forwarder, socket forwarder)
each receive PtyEvent::Output and parse bytes independently
```

### Shadow Screen Consumers

| Consumer | Method | What it reads | File |
|---|---|---|---|
| Client subscribe (TUI) | `snapshot_and_subscribe_cached()` | ANSI snapshot bytes | `agent_handle.rs:404` |
| Client subscribe (WebRTC) | `snapshot_and_subscribe_cached()` | ANSI snapshot bytes | `server_comms.rs:2469,2857,3152` |
| Full snapshot (MCP/Lua) | `get_snapshot()` | Broker RPC already | `agent_handle.rs:482` |
| Cached snapshot | `get_snapshot_cached()` | ANSI from shadow | `agent_handle.rs:275` |
| Dimensions | `dims()` | Grid rows/cols | `agent_handle.rs:607` |
| Cursor visible (Lua) | `cursor_visible()` on PtySessionHandle | DECTCEM mode | `primitives/pty.rs:300` |
| Screen text (MCP) | `get_screen()` on PtySessionHandle | Plain text content | `primitives/pty.rs:364` |
| Resize | `resize_direct()` | Resizes shadow + broker | `agent_handle.rs:622` |
| Ghost creation | `new_ghost()` | Creates shadow-only handle | `primitives/pty.rs:157` |
| Broker scrollback replay | `feed_output()` on PtySessionHandle | Feeds bytes to shadow | `primitives/pty.rs:381` |

### Ghost Session Machinery

Ghost sessions exist solely to hold a shadow screen + routing info after hub restart. The flow:

1. Hub restarts, broker is still running
2. `broker_sessions_recovered` event fires with broker inventory
3. `broker.lua` creates ghost handles via `hub.create_ghost_session()` -- each allocates an AlacrittyParser
4. Broker snapshot replayed into ghost's shadow screen via `feed_output()`
5. Ghost registered in HandleCache + session registry with `_is_ghost = true`
6. `BrokerPtyOutput` frames now route through ghost handle's `feed_broker_output()`

**Files involved in ghost machinery:**

- `cli/src/lua/primitives/pty.rs` -- `PtySessionHandle::new_ghost()`
- `cli/src/lua/primitives/hub.rs` -- `hub.create_ghost_session()` Lua primitive
- `cli/lua/handlers/broker.lua` -- `process_session_manifest()`, `replay_broker_snapshot()`
- `cli/lua/lib/session.lua` -- `register_ghost()`, `is_ghost_entry()`, `info_by_uuid()`, `all_info()`, `has_agent_key()`, `Session.get()`, `Session.resolve_session_uuid()`, `Session.count()`

## Identified Issues and Opportunities

### Critical Issues

1. **Memory duplication**: Every session maintains a 5000-line scrollback AlacrittyParser in both the hub AND the broker. For a hub running 10 sessions, that is 20 parsers.

2. **CPU waste**: Every byte of PTY output is parsed twice through alacritty_terminal's VTE processor -- once in the broker, once in the hub. For high-throughput sessions (build output, test suites), this is measurable.

3. **Complexity**: Ghost sessions add a parallel code path (`_is_ghost` checks scattered through session.lua) that must be maintained alongside real sessions but serves no purpose beyond holding a shadow screen.

### Major Issues

4. **Snapshot inconsistency**: `snapshot_and_subscribe_cached()` uses the hub's local shadow cache, which can drift from the broker's authoritative state if bytes are dropped from the bounded channel (256 frames). The broker is the source of truth, but the attach path avoids it.

5. **Resize race**: `resize_direct()` resizes both the broker PTY AND the hub's shadow screen, but the shadow screen resize is instant while the broker's PTY redraws asynchronously. The shadow screen can briefly show a stale layout at the new dimensions.

### Minor Issues

6. **`feed_output()` on PtySessionHandle**: Only used by ghost replay. With ghosts removed, this method becomes dead code.

7. **`get_scrollback()` alias**: Backwards-compatible alias for `get_snapshot()` on PtySessionHandle -- reads from shadow screen.

## Proposed Refactoring Plan

### Phase 0: Track dimensions and cursor state without the shadow screen

**Goal**: Decouple `dims()`, `kitty_enabled()`, and cursor visibility tracking from the AlacrittyParser so they can survive its removal.

**Changes**:

1. **Dimensions**: `PtyHandle` already has `shared_state: Arc<Mutex<SharedPtyState>>` which stores `dimensions: (u16, u16)`. The `dims()` method currently reads from the shadow screen grid. Change it to read from `shared_state.dimensions` instead. Verify `shared_state.dimensions` is always updated on resize (it is -- `do_resize()` updates it).

   ```rust
   // agent_handle.rs -- replace dims()
   pub fn dims(&self) -> (u16, u16) {
       self.shared_state
           .lock()
           .map(|s| s.dimensions)
           .unwrap_or((24, 80))
   }
   ```

2. **Kitty state**: Already tracked via `kitty_enabled: Arc<AtomicBool>`, updated by `process_pty_bytes()`. No change needed -- this does not read from the shadow screen.

3. **Cursor visibility**: Already tracked via `last_cursor_visible: Arc<Mutex<Option<bool>>>` in PtyHandle and broadcast as `PtyEvent::CursorVisibilityChanged`. The Lua `cursor_visible()` method on `PtySessionHandle` reads from the shadow screen directly. Add an `AtomicBool` to `PtySessionHandle` that is updated by `process_pty_bytes()` and read by `cursor_visible()`.

**Risk**: Low. These are read-path-only changes that replace one correct source with another correct source. Dimensions from `shared_state` are already the canonical write path.

**Test strategy**: Existing tests for `cursor_visible`, `dims`, and `kitty_enabled` should continue to pass. Add a test that verifies `dims()` returns the correct value after resize without touching the shadow screen.

### Phase 1: Route subscribe snapshots through the broker

**Goal**: `snapshot_and_subscribe_cached()` fetches from the broker instead of the local shadow cache.

**Changes**:

1. **Rename current behavior**: `snapshot_and_subscribe_cached()` becomes the new `snapshot_and_subscribe()` -- it always goes to the broker for the snapshot, atomically pairing it with a subscription.

2. **Broker snapshot in attach path**: The subscribe methods in `server_comms.rs` (lines 2469, 2857, 3152) currently call `snapshot_and_subscribe_cached()`. Change these to:
   - Subscribe to `event_tx` first (so no output is missed)
   - Call `get_snapshot()` (which already does the broker RPC)
   - Return (snapshot, subscription)

   The key invariant is that the subscription must be established BEFORE the snapshot is generated, so any output that arrives during snapshot generation is captured by the subscription and not lost. Since the broker's `GetSnapshot` returns a point-in-time ring buffer capture, and the hub continues feeding `PtyEvent::Output` from `BrokerPtyOutput` frames, subscribing first then snapshotting preserves correctness.

3. **Atomicity concern**: The current code locks the shadow screen mutex and subscribes within that lock to prevent races. With broker RPC, we cannot hold a lock across the network call. Instead:
   - Subscribe first: `let rx = event_tx.subscribe()`
   - Then get broker snapshot: `let snapshot = get_snapshot()` (broker RPC)
   - Any output that arrived between subscribe and snapshot-return is in `rx` AND in the snapshot. Clients must tolerate this brief overlap (they already do -- `snapshot_and_subscribe_cached` has the same theoretical issue because `BrokerPtyOutput` processing and subscribe are not truly atomic).

   Actually, the subscribe-then-snapshot ordering means the client might see some bytes twice (once in snapshot, once in the first few `PtyEvent::Output` events). This is already the expected behavior -- terminal emulators are idempotent to duplicate output at the start of a stream. The alternative (snapshot-then-subscribe) risks missing bytes.

4. **Fallback**: If the broker RPC fails, return an empty snapshot with the subscription. The client will see a blank screen that fills in from live output -- better than a crash.

**Risk**: Medium. The broker RPC adds latency to the attach path. Current `snapshot_and_subscribe_cached()` is instantaneous (mutex lock + memcpy). Broker `GetSnapshot` is a synchronous Unix socket RPC -- typically sub-millisecond on localhost, but can spike under load.

**Mitigation**: The attach paths already run inside `tokio::task::spawn_blocking` with a 125ms settle delay. The broker RPC latency is within that budget.

**Test strategy**: The existing `test_get_snapshot_prefers_broker_scrollback_for_broker_backed_sessions` test validates the broker RPC path. Add an integration test that subscribes, gets a broker snapshot, and verifies no output is lost.

### Phase 2: Remove `get_screen()` and `cursor_visible()` shadow screen reads from PtySessionHandle

**Goal**: Eliminate the remaining shadow screen reads on `PtySessionHandle` (the Lua-facing handle).

**Changes**:

1. **`cursor_visible()`**: Replace the shadow screen read with the `AtomicBool` added in Phase 0. `PtySessionHandle` needs access to this atomic, which means either:
   - Adding the atomic to `PtySessionHandle`'s fields (straightforward)
   - Delegating to the `PtyHandle` in the `HandleCache` (requires a lookup)

   Option (a) is cleanest. `PtySessionHandle` already has `shadow_screen`, `kitty_enabled`, `resize_pending` -- adding a `cursor_visible: Arc<AtomicBool>` is consistent.

2. **`get_screen()`**: This returns plain-text terminal contents for MCP tools. Without a shadow screen, two options:
   - **(a)** Add a `GetScreen` broker RPC that returns plain text. This is a new protocol message but follows the existing `GetSnapshot` pattern.
   - **(b)** Keep a lightweight local parser that only tracks visible screen (no scrollback). This defeats the purpose of the refactor.
   - **(c)** Build `get_screen()` from a broker snapshot: request ANSI snapshot, parse it through a temporary AlacrittyParser, extract plain text. Wasteful but avoids protocol changes.

   **Recommendation**: Option (a). The broker already has the authoritative parser. Adding a `GetScreen` RPC (frame type 0x16 or extending HubControl) is a small protocol change. The broker-side implementation is trivial: lock the parser, call `contents()`, return the string.

3. **`feed_output()`**: Remove. Only used by ghost replay, which Phase 3 eliminates.

4. **`get_snapshot()` on PtySessionHandle**: Currently reads from the local shadow screen. Redirect to use the broker RPC via the `BrokerRelay` (same path as `PtyHandle::get_snapshot()`). This requires `PtySessionHandle` to hold a broker connection reference, or delegate to the registered `PtyHandle`.

5. **`get_scrollback()` alias**: Remove alongside `get_snapshot()` on PtySessionHandle, or redirect to broker RPC.

**Risk**: Medium-high. `get_screen()` is used by MCP tooling -- any regression here breaks agent self-awareness (agents reading their own screen). The new broker RPC must be synchronous and fast.

**Test strategy**: Test the new `GetScreen` broker RPC end-to-end. Verify `cursor_visible()` matches the shadow screen's answer before removing the shadow screen (parallel assertion period).

### Phase 3: Remove ghost session machinery

**Goal**: Replace ghost sessions with lightweight routing entries that have no AlacrittyParser.

**Changes**:

1. **Replace `hub.create_ghost_session()`**: Instead of creating a `PtySessionHandle::new_ghost()` (which allocates an AlacrittyParser), create a minimal `PtyHandle` that has:
   - `event_tx` / `subscribe()` (for live output forwarding)
   - `BrokerRelay` (for snapshot, input, resize)
   - `shared_state` with dimensions
   - `kitty_enabled`, `resize_pending`, `cursor_visible` atomics
   - NO `shadow_screen`

   This requires making `shadow_screen` optional in `PtyHandle`. Since Phase 1-2 eliminated all reads from it, this is safe.

2. **Remove `replay_broker_snapshot()`** from `broker.lua`. After hub restart, the ghost handle no longer has a shadow screen to replay into. When a client subscribes, it gets the snapshot from the broker RPC (Phase 1).

3. **Remove `is_ghost_entry()` / `_is_ghost` / `register_ghost()`** from `session.lua`. Ghost entries in the session registry become regular metadata-only entries (plain tables with session info). The distinction was only needed because ghost PtySessionHandles behaved differently from real ones -- without the shadow screen, they are equivalent.

   **Alternative**: Instead of removing `_is_ghost` entirely, keep it as a semantic marker ("this session was recovered, not spawned") without any behavioral difference. This is useful for UI display (showing "recovered" status).

4. **Simplify `Session.get()`, `Session.list()`, `Session.count()`, `Session.all_info()`** etc. -- remove the `is_ghost_entry()` guard clauses. All entries are either real Session instances or plain info tables (for recovered sessions), and both are treated uniformly.

5. **Remove `PtySessionHandle::new_ghost()`** from `primitives/pty.rs`.

6. **Update `hub.create_ghost_session()`** Lua primitive in `primitives/hub.rs` to create a shadow-screen-free `PtyHandle` directly (or rename to `hub.create_recovered_session()` for clarity).

**Risk**: High. This is the most structurally invasive phase. The ghost machinery touches session.lua extensively, and broker.lua's recovery flow is the critical path for hub restart resilience.

**Mitigation**: Deploy Phase 1-2 first. Run with shadow screen still present but unused (asserted via logging) for a burn-in period. Only then proceed with removal.

**Test strategy**:
- `cli/src/lua/runtime.rs` has integration tests for broker recovery flow -- update the mock `create_ghost_session` to return a shadow-screen-free handle.
- Add a test that simulates hub restart: broker has live sessions, hub reconnects, client subscribes and gets correct snapshot from broker RPC.

### Phase 4: Remove shadow_screen from PtyHandle and process_pty_bytes

**Goal**: Final cleanup -- remove the `Arc<Mutex<AlacrittyParser>>` field from `PtyHandle` entirely.

**Changes**:

1. **Make `shadow_screen` optional** in `PtyHandle` (or remove it). All broker-backed handles skip it.

2. **Simplify `process_pty_bytes()`**: Remove the shadow screen update (step 4 in the function). The function becomes:
   - Detect OSC notifications
   - Detect CWD changes, prompt marks
   - Update kitty state (from a new mechanism -- see below)
   - Update cursor visibility (from a new mechanism -- see below)
   - Clear resize-pending
   - Broadcast `PtyEvent::Output`

3. **Kitty and cursor detection without shadow screen**: Currently, `process_pty_bytes()` feeds bytes to the shadow screen and then reads `kitty_enabled()` and `cursor_hidden()` from it. Without the shadow screen, these need to be detected from the raw byte stream:
   - **Kitty keyboard**: Detect `CSI > Nu` (push) and `CSI < u` (pop) sequences in the raw bytes. This is simpler than full VTE parsing -- a regex or state machine over the raw bytes suffices.
   - **Cursor visibility**: Detect `CSI ? 25 h` (show) and `CSI ? 25 l` (hide) DECTCEM sequences.
   - **Alternative**: Keep a minimal VTE parser (no grid, no scrollback) that only tracks mode flags. This is more robust against edge cases (e.g., sequences split across read boundaries) than raw byte scanning.

   **Recommendation**: Use a minimal VTE parser (the `vte` crate's `Parser` with a custom `Perform` impl) that only tracks the two mode flags. This avoids the grid memory overhead while handling sequence splitting correctly. The `vte` crate is already an indirect dependency via `alacritty_terminal`.

4. **Remove `AlacrittyParser` import** from `agent_handle.rs`.

5. **Remove `shadow_screen` from `PtySession::get_direct_access()`** return tuple. Update all call sites.

6. **Test-only handles**: The `#[cfg(test)]` `PtyHandle::new()` constructor uses a shadow screen for test fixtures. Either:
   - Keep it for tests (the shadow screen serves a useful purpose in unit tests)
   - Replace test fixtures with broker-backed mocks

   **Recommendation**: Keep the test-only shadow screen. The refactor targets production code paths only. Test fixtures are not performance-sensitive.

**Risk**: Medium. The kitty/cursor detection from raw bytes is the trickiest part. Sequence splitting across read boundaries is a real concern -- the `vte` crate handles this correctly with its stateful parser.

**Test strategy**: Port existing `process_pty_bytes` tests to use the new detection mechanism. Add edge-case tests for split sequences.

## Risk Assessment and Mitigation

### Showstopper Risks

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| Broker RPC latency spike on attach | Blank screen for seconds | Low | Keep 125ms settle delay. Log slow RPCs. Add timeout with empty-snapshot fallback |
| `get_screen()` broker RPC breaks MCP self-awareness | Agents cannot read own terminal | Medium | Implement and test GetScreen RPC before removing shadow screen |
| Sequence split detection bugs | Incorrect kitty/cursor state | Medium | Use `vte` crate stateful parser, not raw byte scanning |

### Manageable Risks

| Risk | Impact | Likelihood | Mitigation |
|---|---|---|---|
| Ghost removal breaks broker recovery | Sessions lost on hub restart | Low | Extensive integration tests. Staged rollout |
| Duplicate output on subscribe (subscribe-before-snapshot) | Brief visual glitch | High | Terminal emulators handle this gracefully. Already happens today |
| `cursor_visible()` regression | Message delivery timing affected | Low | Parallel assertion period before removing shadow screen |

### Rollback Strategy

Each phase is independently deployable and reversible:

- **Phase 0**: Revert `dims()` to read from shadow screen. No other code depends on the change.
- **Phase 1**: Revert `snapshot_and_subscribe_cached()` to use local shadow cache. Feature flag possible.
- **Phase 2**: Revert `cursor_visible()` and `get_screen()` to shadow screen reads. Broker RPC can stay as an alternative.
- **Phase 3**: Re-enable ghost machinery. Session registry changes are backwards-compatible.
- **Phase 4**: Re-add shadow screen to PtyHandle. This is the hardest to reverse -- do it last.

## Critical Invariants

These must hold true throughout every phase:

1. **A client that subscribes to a session must receive a snapshot that reflects terminal state up to the subscription point, with no gap between snapshot end and live stream start.** (Overlap is acceptable; gap is not.)

2. **`BrokerPtyOutput` frames must continue to flow to all subscribed clients regardless of shadow screen presence.** The `event_tx.send(PtyEvent::Output)` call in `process_pty_bytes()` is the critical path.

3. **Hub restart recovery must produce sessions that clients can subscribe to and receive correct output.** The broker is the source of truth; the hub is a relay.

4. **`cursor_visible()` must reflect the actual cursor state for message delivery timing.** False positives (reporting visible when hidden) cause probe injection during non-input states.

5. **`get_screen()` must return accurate plain-text terminal contents for MCP tool consumption.** This is how agents read their own terminal.

6. **Resize must propagate to the broker AND update local dimension tracking.** The broker controls the actual PTY; the hub tracks dimensions for snapshot coordination.

## Testing Strategy

### Unit Tests (per phase)

- Phase 0: `test_dims_reads_from_shared_state`, `test_cursor_visible_reads_from_atomic`
- Phase 1: `test_subscribe_gets_broker_snapshot`, `test_subscribe_fallback_on_broker_failure`
- Phase 2: `test_get_screen_broker_rpc`, `test_cursor_visible_matches_shadow_screen` (parallel assertion)
- Phase 3: `test_recovery_without_ghost_sessions`, `test_recovered_session_subscribe`
- Phase 4: `test_kitty_detection_from_raw_bytes`, `test_cursor_detection_split_sequence`

### Integration Tests

- Broker recovery flow: hub restart with live broker sessions, client subscribe, verify correct output
- End-to-end subscribe: spawn PTY, write output, client subscribes, verify snapshot + live stream continuity
- MCP get_screen: agent reads own terminal via MCP tool after shadow screen removal

### Regression Tests

- Run the existing test suite (`./test.sh`) after each phase. Key test files:
  - `cli/tests/workspace_store_test.rs` -- workspace manifest correctness
  - `cli/src/hub/agent_handle.rs` -- PtyHandle unit tests
  - `cli/src/lua/primitives/hub.rs` -- register_session broker relay tests
  - `cli/src/tui/runner.rs` -- scrollback event processing
  - `cli/src/lua/runtime.rs` -- broker recovery integration tests

## Success Metrics

1. **Memory**: Each session saves one AlacrittyParser (5000-line scrollback grid) in the hub process. Measurable via RSS before/after with N concurrent sessions.

2. **CPU**: `BrokerPtyOutput` handling drops from ~50us (mutex lock + VTE parse + broadcast) to ~5us (broadcast only) per frame.

3. **Code complexity**: Ghost session machinery removed (~200 lines of Lua, ~80 lines of Rust). `is_ghost_entry()` checks eliminated from 8+ call sites in session.lua.

4. **Correctness**: Snapshot source is always the broker (single source of truth). No more drift between hub shadow and broker parser state.

## Effort Estimates

| Phase | Effort | Complexity | Dependencies |
|---|---|---|---|
| Phase 0: Decouple dims/cursor | 1-2 hours | Low | None |
| Phase 1: Broker snapshot on subscribe | 3-4 hours | Medium | Phase 0 |
| Phase 2: Remove get_screen/cursor_visible shadow reads | 4-6 hours | Medium-High | Phase 1, new broker RPC |
| Phase 3: Remove ghost machinery | 4-6 hours | High | Phases 1-2 |
| Phase 4: Remove shadow_screen from PtyHandle | 3-4 hours | Medium | Phase 3 |

**Total**: 15-22 hours of focused work, split across 5 independently shippable phases.

## Open Questions

1. **Should `get_screen()` become a broker RPC or be removed entirely?** If MCP tools can work with ANSI snapshots (parsed client-side), the RPC is unnecessary. But plain text is significantly more useful for LLM consumption.

2. **Should we keep `_is_ghost` as a semantic marker?** Even without behavioral differences, knowing a session was recovered (not freshly spawned) has UI/logging value.

3. **Is the test-only shadow screen worth keeping?** It simplifies test fixtures but creates a divergence between test and production code paths. The `#[cfg(test)]` branching in PtyHandle is already heavy.

4. **Should Phase 1 use a timeout on the broker snapshot RPC?** Currently `get_snapshot()` blocks indefinitely on the Unix socket. A timeout with empty-snapshot fallback would improve resilience.
