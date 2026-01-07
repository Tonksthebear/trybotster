# Migration Status: CLI Refactoring

## ✅ COMPLETED

**Final State (2025-01):**

| File | Lines | Started | Reduction | Notes |
|------|-------|---------|-----------|-------|
| hub/mod.rs | 557 | 1338 | -781 (58%) | Central orchestrator, thin wrappers |
| hub/actions.rs | 607 | 224 | +383 | Action dispatch + spawn helpers |
| hub/state.rs | 371 | 180 | +191 | State mgmt + worktree loading |
| hub/polling.rs | 390 | 286 | +104 | Heartbeat + notifications |
| relay/browser.rs | 368 | 0 | +368 | Browser event handling |
| agent/mod.rs | 1007 | 1056 | -49 (5%) | Agent core, delegates to submodules |
| agent/pty/mod.rs | 320 | 291 | +29 | Added resize_with_clear |
| agent/scroll.rs | 65 | 0 | +65 | Scroll operations |

**Tests: 266 passing**

---

## All Phases Complete

### Phase 1: Extract handle_action() to hub/actions.rs ✓
Moved 237-line match block + helpers to `actions::dispatch()`.

### Phase 2: Create relay/browser.rs ✓
Moved ~280 lines of browser event handling to relay module.

### Phase 3: Move load_available_worktrees to hub/state.rs ✓
Delegated 65 lines to `HubState::load_available_worktrees()`.

### Phase 4: Consolidate spawn methods ✓
Moved spawn helpers to actions.rs.

### Phase 5: Clean up poll_messages ✓
Extracted `try_notify_existing_agent` helper.

### Phase 6: Move resize_pty_session to pty ✓
Now `pty::resize_with_clear()`.

### Phase 7: Move send_heartbeat to hub/polling.rs ✓
Created `polling::send_heartbeat_if_due()`.

### Phase 8: Move poll_agent_notifications to hub/polling.rs ✓
Created `polling::poll_and_send_agent_notifications()`.

### Phase 9: Extract Agent scroll methods to agent/scroll.rs ✓
Created `scroll::{is_scrolled, get_offset, up, down, to_bottom, to_top}`.

### Phase 10: Final cleanup ✓
- All 266 tests passing
- Clippy shows only pre-existing warnings
- Line counts verified

### Phase 11: Remove dead code ✓
Removed unused `write_to_active_pty` method (duplicate of `write_input`). -18 lines.

---

## Architecture Summary

```text
hub/mod.rs (557 lines)
├── Hub struct + new()
├── Simple accessors
├── tick() - calls polling functions
├── poll_messages() - server communication
├── setup/registration wrappers
└── Tests

hub/actions.rs (607 lines)
├── HubAction enum
├── dispatch() - all action handling
├── handle_menu_select()
├── handle_input_submit()
├── spawn_agent_from_worktree()
├── create_and_spawn_agent()
└── spawn_agent_with_tunnel()

hub/polling.rs (390 lines)
├── PollingConfig struct
├── poll_messages()
├── acknowledge_message()
├── send_heartbeat()
├── send_heartbeat_if_due() ← Hub delegates here
├── poll_and_send_agent_notifications() ← Hub delegates here
└── send_agent_notification()

relay/browser.rs (368 lines)
├── poll_events()
├── handle_input()
├── send_agent_list()
├── send_worktree_list()
├── create_agent()
├── reopen_worktree()
└── send_output()

agent/mod.rs (1025 lines)
├── Agent struct + new()
├── Scroll methods → delegate to scroll.rs
├── PTY methods
├── spawn() → delegates to spawn.rs
└── Tests (531 lines)

agent/scroll.rs (65 lines)
├── is_scrolled()
├── get_offset()
├── up()
├── down()
├── to_bottom()
└── to_top()
```

---

## Architectural Decisions

1. **Browser logic belongs in `relay/`, NOT `hub/`**
2. **Tests stay in-file** - Don't extract to separate files
3. **Use thin wrappers** - Hub/Agent methods delegate to specialized modules
4. **PollingConfig pattern** - Pass config struct, not individual params

---

## Notes for Context Recovery

- hub/mod.rs reduced by 58% (1338 → 557 lines)
- Total extraction: ~900 lines moved to specialized modules
- All tests continue to pass
- No breaking API changes - existing method signatures preserved
