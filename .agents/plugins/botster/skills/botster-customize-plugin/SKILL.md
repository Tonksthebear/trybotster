---
name: botster-customize-plugin
description: Use when creating or modifying a Botster Lua plugin with hooks, MCP tools, prompts, secrets, timers, HTTP, UI, or plugin.db persistence.
---

# Botster Customize Plugin

Create reusable Botster behavior as a Lua plugin when it should be shared,
hot-reloaded, scoped, or distributed.

## Locations

- Device release: `~/.botster/plugins/<name>/init.lua`
- Device debug: `~/.botster-dev/plugins/<name>/init.lua`
- Repo-specific: `<repo>/.botster/plugins/<name>/init.lua`

New plugin directories may require one hub restart before hot-reload watches
them. Existing plugin files hot-reload on save.

## Basic Shape

```lua
local hooks = require("hub.hooks")

local db = plugin.db{
  version = 1,
  models = {
    events = {
      id = true,
      kind = { "text", required = true },
      payload = { "text", required = true },
      created_at = { "integer", required = true },
    },
  },
}

mcp.tool("my_tool", {
  description = "Do one plugin action",
  input_schema = {
    type = "object",
    properties = {
      value = { type = "string" },
    },
    required = { "value" },
  },
}, function(params, context)
  db.events:insert{
    kind = "my_tool",
    payload = json.encode(params),
    created_at = os.time(),
  }
  return { ok = true, session_uuid = context.session_uuid }
end)

return {}
```

## Persistence

Call `plugin.db{}` at plugin load time and capture the handle in a local. Use it
for plugin-owned durable state: queues, ledgers, workflow stages, sync cursors,
and audit records.

Do not persist PTY delivery mechanics in plugin DB. PTY probing and immediate
delivery queues belong to runtime state.

## Available Building Blocks

Core primitives:

- `log`
- `json`
- `fs`
- `config`
- `secrets`

Event-driven primitives:

- `webrtc`
- `tui`
- `socket`
- `pty`
- `hub`
- `connection`
- `worktree`
- `events`
- `http`
- `timer`
- `watch`
- `websocket`
- `action_cable`
- `hub_discovery`
- `hub_client`
- `mcp`
- `update`
- `push`

Hook observers:

- `agent_created`
- `agent_deleted`
- `agent_lifecycle`
- `_pty_notification_raw`
- `pty_notification`
- `pty_title_changed`
- `pty_cwd_changed`
- `pty_prompt`
- `pty_input`
- `client_connected`
- `client_disconnected`
- `after_agent_create`
- `before_agent_close`
- `after_agent_close`
- `shutdown`

Hook interceptors:

- `before_agent_create`
- `before_agent_delete`
- `before_client_subscribe`
- `filter_agent_env`

## MCP Surface

Expose small, stable tools with clear schemas. Prefer structured return tables
over prose strings when other tools or agents may consume the result.

Register prompts only for instructions that are genuinely reusable. Agent-side
skills should carry static workflow guidance when possible.

## References

- `docs/lua/primitives.md` — primitive APIs and execution model.
- `docs/lua/hook-system.md` — hook APIs, events, and Rust bridge callbacks.
- `docs/lua/plugin-db.md` — `plugin.db{}` schema, migrations, constraints.
- `docs/lua/directory-structure.md` — plugin paths and override order.
- `docs/lua/hot-reload.md` — plugin reload behavior.
- `app/templates/plugins/` — working plugin templates.
