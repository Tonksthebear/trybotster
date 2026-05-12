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
- `lib.notifications` for scoped PTY notification observation and ownership

## Session Actions, Agent Spawns, And Command Gates

Use `lib.session_actions` for plugin-owned per-session affordances. Core
publishes action descriptors as `session_action` entities, and every client
invokes them through `execute_session_action`.

Action handlers run on the hub command path. They should validate, update
plugin-owned state, queue work, and return promptly. Do not resolve executables
or write generated config files directly in an action handler. Use
`hub.prepare_plugin_command({
  request_id,
  command,
  config_path?,
  config_contents?,
  context?,
})` to offload PATH/filesystem preparation to the blocking worker pool, then
continue from `events.on("plugin_command_prepared", ...)`.

Completion events include:

- `request_id` — plugin token used to ignore stale completions.
- `command` and `config_path` on success.
- `context` — the opaque table passed by the plugin.
- `error_kind` — `command_blank`, `command_missing`,
  `config_write_failed`, or `task_failed`.
- `error` — human-readable message.

Store the active request token in plugin-owned state and drop completion events
whose `request_id` is no longer current. This covers disable/retry/reload races
without adding provider-specific behavior to core.

Use `hub.run_command_gate({...})` when a plugin needs a one-shot command result
such as a test, lint, or build gate. It composes with
`hub.prepare_plugin_command`, runs off the hub event loop, captures bounded
stdout/stderr tails, hard-kills timed-out process groups, and emits
`command_gate_completed`.

```lua
hub.run_command_gate{
  request_id = "gate-123",
  command = "./cli/test.sh --unit -- plugin_helpers",
  cwd = "/repo/cli",
  timeout_secs = 600,
  metadata = { ticket_id = "T-42", gate_id = "tests" },
  context = { run_id = "R-9" },
}

events.on("command_gate_completed", function(event)
  if event.request_id ~= "gate-123" then return end
  -- event.success, event.exit_status, event.stdout_tail,
  -- event.stderr_tail, event.error_kind, event.error
end)
```

Keep `metadata` and `context` small, JSON-serializable, and correlation-only.
They are echoed back to plugin events; do not include secrets or unbounded
command output.

Use `Hub.get():create_agent({...})` for plugin-owned agent sessions. Always use
or persist the returned `request_id`; it is echoed through lifecycle events so
async worktree creation can be correlated with the final `session_uuid`.

```lua
local Hub = require("lib.hub")

local result = Hub.get():create_agent{
  target_id = "tgt_...",
  agent_name = "codex",
  label = "T-42 implementer",
  prompt = "Implement the ticket",
  request_id = "spawn-T-42-worker",
  metadata = {
    owner_plugin = "projects",
    visibility = "plugin",
    surface = "projects",
    ticket_id = "T-42",
    run_id = "R-9",
    gate_id = "implementation",
    role = "worker",
  },
}
```

Recover plugin-owned sessions after reload with
`Hub.get():list_owned_sessions("projects")`. It returns the same session-array
shape for local and remote hubs.

## Notification Policy Ownership

Use `lib.notifications` when a plugin needs to observe, suppress, or replace
PTY notification delivery for sessions it owns. Do not intercept
`pty_notification` for suppression; by the time that hook fires, core delivery
has already been accepted. The notification policy path is:

```text
_pty_notification_raw -> enrichment -> lib.notifications -> pty_notification
```

Observers watch matching notification intents without changing default behavior.
Claims take ownership for matching intents and return a declarative decision.
The hub still owns matching, timeouts, fallback behavior, badge mutation, web
push, and transient UI events. Plugin handlers run in the plugin worker.

```lua
local notifications = require("lib.notifications")

notifications.observe({
  name = "projects.audit_notifications",
  scope = { owner_plugin = "projects" },
  phase = "both", -- "before", "after", or "both"
  handler = function(phase, intent, decision)
    log.info(string.format(
      "notification %s for %s: %s",
      phase,
      tostring(intent.session_uuid),
      tostring(decision and decision.core or "pending")
    ))
  end,
})

notifications.claim({
  name = "projects.pipeline_notifications",
  scope = { owner_plugin = "projects" },
  handler = function(intent)
    if intent.message and intent.message:find("handled by pipeline", 1, true) then
      return { core = "suppress", reason = "projects_pipeline_handled" }
    end

    return {
      core = "replace", -- "default", "suppress", or "replace"
      reason = "projects_pipeline_alert",
      custom = {
        kind = "projects_alert",
        title = "Pipeline needs review",
        body = intent.message or intent.body,
        push = true,
        transient = true,
        badge = true,
      },
    }
  end,
})
```

Scope options:

- `session_uuid = "sess-..."` — one exact session.
- `sessions = { "sess-a", "sess-b" }` — explicit session set.
- `owner_plugin = "projects"` — sessions spawned with matching
  `metadata.owner_plugin`.
- `surface = "projects"` — sessions bound to a plugin surface.
- `all_sessions = true` — every session notification.

All-session scopes are powerful and require explicit capability declarations:

```lua
notifications.observe({
  name = "auditor.all_notifications",
  scope = { all_sessions = true },
  capabilities = { "notifications.global_observe" },
  handler = function(phase, intent, decision) end,
})

notifications.claim({
  name = "policy.all_notifications",
  scope = { all_sessions = true },
  capabilities = { "notifications.global_claim" },
  handler = function(intent)
    return { core = "default" }
  end,
})
```

Use global observe for audit/telemetry and global claim only for plugins that
are intentionally replacing Botster's default notification policy. A failed
claim falls back to default behavior.

Hook observers:

- `agent_created`
- `agent_deleted`
- `agent_lifecycle`
- `_pty_notification_raw` — internal raw signal before enrichment and policy.
- `pty_notification` — final delivery after notification policy accepts.
- `pty_notification_suppressed` — final suppression after notification policy.
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

Event observers:

- `plugin_command_prepared` — completion for `hub.prepare_plugin_command`.
- `command_gate_completed` — completion for `hub.run_command_gate`; includes
  `request_id`, `success`, `exit_status`, bounded output tails, optional
  `metadata`/`context`, and error fields.

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

## Web Surfaces

Use `lib.surfaces` when a plugin needs a routable browser interface. Core owns
sidebar placement, so plugins should declare navigation metadata instead of
patching workspace layouts.

```lua
local surfaces = require("lib.surfaces")

surfaces.register("vault", {
  label = "Vault",
  icon = "book-open", -- Heroicons mini filename without .svg
  nav = { section = "workspace", order = 25 },
  sidebar = { surface = "vault_sidebar" },
  routes = {
    { path = "/", render = vault_home },
    { path = "/sessions/:session_uuid", render = vault_session },
    { path = "/graph", layout = "fullscreen", render = vault_graph },
  },
})
```

Set `nav = false` for routable utility/debug surfaces that should not appear
in the core Plugins section. Icon names are Heroicons mini filenames from
`app/assets/svg/icons/heroicons/mini`, without the `.svg` suffix.

When a plugin has its own session/workspace structure, declare a route-scoped
sidebar instead of nesting navigation in the main page. Botster renders the
plugin name with a back button, then mounts the named sidebar surface while the
user is inside the plugin route:

```lua
surfaces.register("vault_sidebar", {
  render = function()
    return ui.stack{
      ui.session_list{
        visibility = "plugin",
        owner_plugin = "vault",
        surface = "vault",
      },
    }
  end,
})
```

### Plugin-Owned Sessions

When a plugin owns sessions that should not appear as normal workspace sessions,
spawn them with ownership metadata:

```lua
metadata = {
  owner_plugin = "vault",
  visibility = "plugin",
  surface = "vault",
}
```

Then render them inside the plugin surface:

```lua
ui.session_list{
  visibility = "plugin",
  owner_plugin = "vault",
  surface = "vault",
}
```

For terminal views inside a plugin route, use `ui.session_terminal` rather than
linking to the global `/sessions/:session_uuid` route:

```lua
ui.session_terminal{
  session_uuid = state.params.session_uuid,
  back = ctx.path("/"),
}
```

Notification URLs for plugin-owned sessions route through
`/hubs/<hub>/<surface>/sessions/<session_uuid>` when the session has
`visibility = "plugin"` and a matching `surface`/`owner_plugin`.

### Form Primitives

Use the shared Lua form primitives for plugin surfaces before reaching for a
custom iframe. The v1 public set is intentionally small:

- `ui.text_input`
- `ui.textarea`
- `ui.checkbox`
- `ui.select`

Web rendering is Catalyst-first. Do not create raw custom form controls in the
parent app. TUI rendering is operational and compact: `textarea` is
read-only/display-only in TUI v1, so use `text_input` for editable TUI text
entry until multiline editing lands.

### Custom HTML Views

Use `lib.plugin_assets` plus `ui.iframe` when a plugin needs a fully custom
HTML/CSS/JS interface, such as a generated graph or drag-and-drop board. Do not
inject raw HTML into the parent Botster app.

```lua
local plugin_assets = require("lib.plugin_assets")

local board_url = plugin_assets.expose_file("kanban_board", "/path/to/board.html", {
  content_type = "text/html",
})

plugin_assets.on_message("card.move", function(payload, ctx)
  -- Validate payload and update plugin-owned state.
end)

ui.iframe{
  src = board_url,
  title = "Board",
  sandbox = "allow-scripts",
  bridge = { actions = { "card.move" } },
}
```

Register iframe/canvas/editor routes with `layout = "fullscreen"` so Botster
uses the same full-height, no-padding shell as terminal routes:

```lua
routes = {
  { path = "/board", layout = "fullscreen", render = render_board },
}
```

Iframe JavaScript can post:

```js
window.parent.postMessage({
  type: "botster.plugin_action",
  action: "card.move",
  payload: { card_id: "c1", to_column: "done", position: 2 },
}, "*")
```

Only actions declared in `bridge.actions` are forwarded to the hub. Keep the
iframe sandboxed by default and use self-contained HTML unless you also expose
supporting assets deliberately.

## References

- `docs/lua/primitives.md` — primitive APIs and execution model.
- `docs/lua/hook-system.md` — hook APIs, events, and Rust bridge callbacks.
- `docs/lua/plugin-db.md` — `plugin.db{}` schema, migrations, constraints.
- `docs/lua/directory-structure.md` — plugin paths and override order.
- `docs/lua/hot-reload.md` — plugin reload behavior.
- `catalog/templates/plugins/` — working plugin templates.
