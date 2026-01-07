# Migration Status: BotsterApp → Hub

## Summary

**MIGRATION COMPLETE** - The CLI has been fully refactored to use the Hub architecture.

- `main.rs` reduced from **3511 lines to 759 lines** (78% reduction)
- BotsterApp and run_interactive() have been deleted
- All 258 tests pass

## Architecture

```
              ┌──────────────────────┐
              │        Hub           │
              │  - Owns all state    │
              │  - Runs tick()       │
              │  - handle_action()   │
              └──────────┬───────────┘
                         │
          ┌──────────────┼──────────────┐
          │              │              │
          ▼              ▼              ▼
    tui::render()   Hub::tick()    Browser Relay
    (renders)       (poll/heartbeat) (events)
```

## Module Structure

```
hub/
  mod.rs         - Hub struct with all fields and methods
  state.rs       - HubState for agent/worktree management
  lifecycle.rs   - spawn_agent, close_agent
  actions.rs     - HubAction enum

tui/
  mod.rs         - Module definition
  guard.rs       - TerminalGuard RAII
  qr.rs          - QR code generation
  input.rs       - event_to_hub_action
  view.rs        - ViewState, ViewContext
  render.rs      - Main render() function

relay/
  mod.rs         - Re-exports
  events.rs      - browser_event_to_hub_action

server/
  messages.rs    - message_to_hub_action
```

## Entry Points

| Function | Description |
|----------|-------------|
| `main()` | CLI parsing, calls run_with_hub() |
| `run_with_hub()` | Main event loop using Hub |
| `run_headless()` | Headless mode (not yet implemented) |

## Event Loop (run_with_hub)

1. Poll keyboard events → `tui::event_to_hub_action()` → `hub.handle_action()`
2. Handle browser resize
3. Render with `tui::render()`
4. Poll browser events → `hub.handle_action()` for each
5. Send browser output
6. `hub.tick()` for periodic tasks (poll_messages, send_heartbeat, poll_agent_notifications)
