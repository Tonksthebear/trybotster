---
name: botster-customize-hub
description: Use when adding Botster hub hooks, commands, lifecycle behavior, background tasks, or custom orchestration logic.
---

# Botster Customize Hub

Hub behavior belongs in one of these places:

- `~/.botster/lua/user/init.lua` for device-wide one-off behavior.
- `~/.botster-dev/lua/user/init.lua` for debug builds.
- `~/.botster/plugins/<name>/init.lua` for reusable device plugins.
- `<repo>/.botster/plugins/<name>/init.lua` for repo-specific plugins.

The hub is the central orchestrator. Keep policy and coordination in hub/plugin
code, not in one agent's private scratch state.

## Hooks

Use observers for fire-and-forget reactions:

```lua
local hooks = require("hub.hooks")

hooks.on("after_agent_create", "my_plugin.after_agent_create", function(agent)
  log.info("Agent started: " .. agent.session_uuid)
  agent:set_meta("started_at", os.time())
end)
```

Use interceptors when the hook must allow, modify, or block the action:

```lua
hooks.intercept("before_agent_create", "my_plugin.guard", function(params)
  if not params.branch_name then
    return nil
  end
  return params
end, { timeout_ms = 50 })
```

### Available Observer Hooks

- `agent_created` — agent spawned; broadcasts to all clients.
- `agent_deleted` — agent removed; broadcasts to all clients.
- `agent_lifecycle` — lifecycle stage changes.
- `_pty_notification_raw` — internal raw notification enrichment.
- `pty_notification` — web push notification hook.
- `pty_title_changed` — OSC 0/2 title changed.
- `pty_cwd_changed` — OSC 7 cwd changed.
- `pty_prompt` — OSC 133/633 prompt marks.
- `pty_input` — user typed into PTY.
- `client_connected` — client joined registry.
- `client_disconnected` — client left registry.
- `after_agent_create` — after `Agent.new()` completes.
- `before_agent_close` — before sessions are killed.
- `after_agent_close` — after agent is removed.
- `shutdown` — hub shutting down.

### Available Interceptor Hooks

- `before_agent_create` — transform params or return nil to block creation.
- `before_agent_delete` — transform params or return nil to block deletion.
- `before_client_subscribe` — transform or block subscriptions.
- `filter_agent_env` — modify PTY session environment variables.

### Rust-To-Lua Events

Use `events.on(event, fn(data))` for Rust-emitted events:

- `command_message`
- `worktree_created`
- `worktree_create_failed`
- `connection_code_ready`
- `connection_code_error`
- `agent_status_changed`
- `process_exited`
- `outgoing_signal`

## Commands

Register hub commands for command palette and tool-driven workflows:

```lua
commands.register("notify-slack", function(client, sub_id, command)
  log.info("notify-slack invoked")
end, { description = "Send Slack notification" })
```

## Rules

- Use async table-first primitives inside callbacks.
- Do not call blocking sync primitives after the hub event loop starts.
- Store durable plugin state in `plugin.db{}` during plugin load.
- Use `session_uuid` as the routing key.
- Keep plugins generic; Botster should stay agent-CLI agnostic.

## References

- `docs/lua/hook-system.md` — complete hook/event catalog and bridge methods.
- `docs/lua/primitives.md` — all Lua primitives and blocking rules.
- `docs/lua/directory-structure.md` — config resolution and plugin locations.
- `docs/lua/plugin-db.md` — durable per-plugin SQLite schema and migrations.
