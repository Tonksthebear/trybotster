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
- `after_hub_command` — hub command finished; includes success/error.
- `after_agent_create` — after `Agent.new()` completes.
- `before_agent_close` — before sessions are killed.
- `after_agent_close` — after agent is removed.
- `shutdown` — hub shutting down.

### Available Interceptor Hooks

- `before_agent_create` — transform params or return nil to block creation.
- `before_agent_delete` — transform params or return nil to block deletion.
- `before_hub_command` — transform or block a raw command envelope.
- `before_command` — transform or block a registered command context.
- `before_client_subscribe` — transform or block subscriptions.
- `filter_agent_env` — modify PTY session environment variables.

### Rust-To-Lua Events

Use `events.on(event, fn(data))` for Rust-emitted events:

- `worktree_created`
- `worktree_create_failed`
- `connection_code_ready`
- `connection_code_error`
- `agent_status_changed`
- `process_exited`
- `plugin_command_prepared` — completion for `hub.prepare_plugin_command`;
  includes `request_id`, optional `command`/`config_path`, opaque `context`,
  `error_kind`, and `error`.
- `command_gate_completed` — completion for `hub.run_command_gate`; includes
  `request_id`, `success`, `exit_status`, `duration_ms`, bounded
  `stdout_tail`/`stderr_tail`, optional `metadata`/`context`, `error_kind`,
  and `error`.
- `url_probe_ready` — completion for `hub.probe_url_ready`.
- `outgoing_signal`

## Commands

All commands enter through `cli/lua/lib/client.lua`. Browser, TUI, socket, MCP,
hub-to-hub, GitHub, and Rails-originated commands should dispatch through a real
client transport or `lib.internal_client`; do not add side-channel command
events. `create_agent` is an explicit spawn operation: do not infer reuse from
matching workspace, target, issue, or branch metadata.

Register hub commands for command palette and tool-driven workflows:

```lua
commands.register("notify-slack", function(client, sub_id, command)
  log.info("notify-slack invoked")
end, { description = "Send Slack notification" })
```

## Plugin-Orchestration Helpers

For plugin-owned agent sessions, prefer the table-style hub API:

```lua
local Hub = require("lib.hub")

local result = Hub.get():create_agent{
  target_id = "tgt_...",
  agent_name = "codex",
  label = "Workflow worker",
  prompt = "Do the assigned work",
  request_id = "spawn-run-123",
  metadata = {
    owner_plugin = "projects",
    visibility = "plugin",
    surface = "projects",
    run_id = "run-123",
    role = "worker",
  },
}
```

`request_id` is the correlation token. If omitted, Botster mints a
hub-prefixed `msg_...` ID. It is echoed through create/lifecycle/worktree paths
so plugins can recover async worktree creation. Use
`Hub.get():list_owned_sessions(owner_plugin)` after reload to rebuild a
plugin's in-flight session projection.

Use `hub.run_command_gate({...})` for one-shot command gates. It composes with
`hub.prepare_plugin_command`, executes off the event loop, captures bounded
output tails, and emits `command_gate_completed`. Keep `metadata`/`context`
small and correlation-only because they are echoed into the completion event.

## Rules

- Use async table-first primitives inside callbacks.
- Do not call blocking sync primitives after the hub event loop starts.
- Use `hub.prepare_plugin_command` for plugin command PATH/config preparation
  that would otherwise block action handlers.
- Use `hub.run_command_gate` for test/build/lint gates that need captured exit
  status and bounded output without a PTY session.
- Store durable plugin state in `plugin.db{}` during plugin load.
- Use `session_uuid` as the routing key.
- Keep plugins generic; Botster should stay agent-CLI agnostic.

## References

- `docs/lua/hook-system.md` — complete hook/event catalog and bridge methods.
- `docs/lua/primitives.md` — all Lua primitives and blocking rules.
- `docs/lua/directory-structure.md` — config resolution and plugin locations.
- `docs/lua/plugin-db.md` — durable per-plugin SQLite schema and migrations.
