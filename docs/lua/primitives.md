# Botster Lua Primitives Reference

21 Rust modules are registered as Lua globals, plus `mcp` which is a pure Lua module. Core primitives load unconditionally; event-driven primitives require a HubEventSender.

## Calling Conventions & Execution Models

Primitives expose different calling patterns to control blocking semantics:

- **Async (table-first, callbacks):** `http.request({...}, callback)`, `websocket.connect(url, {on_message=fn})`, `action_cable.subscribe(conn, ch, params, callback)` — non-blocking, safe everywhere. Callbacks run asynchronously when events arrive.
- **Sync (positional shortcuts):** `http.get(url)`, `http.post(url, body)` — BLOCKING the event loop. Only safe at plugin load time (before hub starts). Using in callbacks breaks WebRTC health checks.
- **Dedicated threads:** `websocket`, `action_cable` — spawn dedicated OS/async threads for I/O, keeping the hub event loop responsive.

**Critical invariant:** The hub event loop runs inside tokio's `block_on()`. Any blocking operation (sync HTTP, file I/O) stalls the entire hub, preventing `dc_pong` responses from reaching connected web clients within 30 seconds. The client times out and closes the WebRTC connection. Always use async forms in callbacks and runtime code.

## Core Primitives (no HubEventSender needed)

### `log`
```lua
log.info(msg)
log.warn(msg)
log.error(msg)
log.debug(msg)
```

### `json`
```lua
json.encode(table) -> string
json.decode(str) -> table
```

### `fs`
```lua
fs.read(path) -> string
fs.write(path, content)
fs.exists(path) -> bool
fs.is_dir(path) -> bool
fs.listdir(path) -> table
fs.copy(src, dst)
fs.stat(path) -> {size, modified, is_dir, ...}
fs.mkdir(path)
fs.rmdir(path)
fs.delete(path)
fs.rename(from, to)
fs.resolve_safe(root, rel) -> path  -- path traversal protection
```

### `config`
```lua
config.get(key) -> value
config.all() -> table
config.set(key, val)
config.env(name) -> string       -- environment variable access
config.lua_path() -> string      -- Lua script base path
config.data_dir() -> string      -- config directory
config.template_catalog_path() -> string|nil -- explicit local template catalog root
```

### `secrets`
```lua
secrets.get(key) -> string       -- plugin-scoped AES-GCM encrypted storage
secrets.set(key, val)
secrets.delete(key)
```

## Plugin Web Surfaces

Plugins register web client surfaces through `lib.surfaces`. A registered surface
is routable at `/hubs/<hub_id>/<surface_name>` unless it explicitly opts out of
navigation.

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

Set `nav = false` for routable surfaces that should stay out of the sidebar.
Icon names are Heroicons mini filenames vendored under
`app/assets/svg/icons/heroicons/mini`.

Set `sidebar = { surface = "vault_sidebar" }` when the plugin should own the
left sidebar while the user is inside that surface. Botster renders the plugin
name and a back button at the top, then mounts the named sidebar surface. The
sidebar surface can render plugin-local session/workspace navigation without
nested nav inside the main page:

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

Plugin-scoped sessions should carry explicit ownership metadata so core can
hide them from the default workspace list while the owning plugin can render
them:

```lua
metadata = {
  owner_plugin = "vault",
  visibility = "plugin",
  surface = "vault",
}
```

Surface UI can render those sessions and mount the core terminal viewer without
leaving the plugin route:

```lua
ui.session_list{
  visibility = "plugin",
  owner_plugin = "vault",
  surface = "vault",
}

ui.session_terminal{
  session_uuid = state.params.session_uuid,
  back = ctx.path("/"),
}
```

Shared form controls are available in plugin surfaces through the same
renderer-agnostic `ui` contract. Use semantic props only; browser details such
as CSS classes or DOM attributes are not part of the Lua contract. Change
events are action envelopes, and renderers merge the next value into the action
payload:

```lua
ui.form{
  children = {
    ui.text_input{
      id = "project-name",
      label = "Project name",
      required = true,
      value = project.name,
      placeholder = "Name",
      on_change = ui.action("workflow.project.name.change", { project_id = project.id }),
    },
    ui.textarea{
      id = "project-notes",
      label = "Notes",
      required = true,
      value = project.notes,
      on_change = ui.action("workflow.project.notes.change", { project_id = project.id }),
    },
    ui.checkbox{
      id = "requires-review",
      label = "Requires review",
      selected = project.requires_review,
      on_change = ui.action("workflow.project.review.toggle", { project_id = project.id }),
    },
    ui.select{
      id = "priority",
      label = "Priority",
      required = true,
      value = project.priority,
      options = {
        { value = "normal", label = "Normal" },
        { value = "high", label = "High" },
      },
      on_change = ui.action("workflow.project.priority.change", { project_id = project.id }),
    },
    ui.button{
      label = "Save",
      action = ui.action("workflow.project.save", { project_id = project.id }),
    },
  },
}
```

`required = true` is supported on `ui.text_input`, `ui.textarea`, and
`ui.select`. It is renderer metadata for accessibility, styling, and native web
control attributes. Put related controls and the submit-style `ui.button` inside
`ui.form` to make the web renderer run native validity checks before dispatching
the button action. This is only client-side UX; always keep plugin-side or
hub-side validation for required fields.

Submit-style `ui.button` and `ui.icon_button` actions use the generic
`ui_action` lifecycle. The browser generates an `action_request_id` when the
user activates the control, marks that submitter pending immediately, and sends
the id beside the action envelope:

```json
{
  "type": "ui_action",
  "target_surface": "project_pipelines",
  "action_request_id": "ua_...",
  "envelope": { "id": "workflow.project.save", "payload": { "project_id": "p1" } }
}
```

Handlers return `action.HANDLED` for a generic success,
`action.result{ message = "Saved" }` for specific result text,
`action.result{ message = "Saved", navigate = { label = "Open", path =
"/pipelines/tickets/t1" } }` for a follow-up navigation affordance, or
`action.result{ ok = false, error = "Select a target." }` for errors. The
hub replies on the same subscription with:

```json
{
  "type": "ui_action_result",
  "v": 1,
  "target_surface": "project_pipelines",
  "action_request_id": "ua_...",
  "action_id": "workflow.project.save",
  "ok": true,
  "handled": true,
  "via": "handler",
  "message": "Saved"
}
```

This feedback path is renderer-owned. Do not add CSS, DOM attributes, or
web-only pending state to Lua payloads.

`ui.checkbox` does not accept `required` yet; required checkbox semantics mean
"must be checked", so model that as domain validation until the shared contract
adds a checked-required checkbox state.

TUI v1 renders `ui.textarea` as read-only/display-only text. The web renderer
uses the Catalyst textarea and emits `on_change`; TUI authors who need editable
text entry should use `ui.text_input` until multiline editing lands in the TUI
adapter.

Plugins can also expose generated static HTML through a sandboxed iframe. The
web client fetches `botster-plugin-asset://...` sources over the existing hub
connection and mounts them as blob URLs, so remote/tunneled clients do not
need access to the user's `localhost` or `file://` paths:

```lua
local plugin_assets = require("lib.plugin_assets")

local graph_url = plugin_assets.expose_file("knowledge_graph", "/Users/me/knowledge/ops/graph.html", {
  content_type = "text/html",
})

plugin_assets.on_message("card.move", function(payload, ctx)
  -- Validate payload and update plugin state.
end)

ui.iframe{
  src = graph_url,
  title = "Knowledge graph",
  sandbox = "allow-scripts",
  bridge = { actions = { "card.move" } },
}
```

Use `layout = "fullscreen"` on a route that needs full-bleed space, such as an
iframe, canvas, dashboard, editor, or embedded terminal-like tool. Fullscreen
routes use the same padded-shell bypass as session terminal routes.

Iframe content can call `window.parent.postMessage({ type =
"botster.plugin_action", action = "card.move", payload = {...} }, "*")`.
Only actions declared in the iframe `bridge.actions` list are forwarded to the
hub, and Lua receives them through `plugin_assets.on_message`.

## Plugin Session Actions

Plugins publish per-session capabilities through `lib.session_actions`, not
provider-specific browser commands. Core broadcasts each descriptor as a
`session_action` entity, and clients invoke the selected capability through the
generic `execute_session_action` hub command. See
[`docs/lua/session-actions.md`](session-actions.md) for the descriptor shape and
registration contract.

## Plugin-Owned Entities

Plugins publish durable dynamic state through the Hub entity API backed by
`lib.entity_broadcast`. This is the only shared model-state path for plugin UI;
do not add client-specific refresh commands or per-plugin collection snapshots.
`ui_tree_snapshot` frames describe presentation, while entity frames carry the
data that both browser and TUI stores consume.

Plugin entity names must be `<plugin>.<type>`, and the `<plugin>` prefix must
match the plugin that registers the type. Built-in names such as `session`,
`workspace`, `spawn_target`, `worktree`, `hub`, `connection_code`, `template`,
and `session_action` are reserved. Plugin records must use `id` as a non-empty
string id field so generic browser and TUI entity stores can consume them
without per-plugin wiring.

```lua
local EB = require("lib.entity_broadcast")
local Hub = require("lib.hub")

local boards = {
  { id = "board-1", name = "Roadmap", status = "active" },
}

EB.register("kanban.board", {
  id_field = "id",
  owner_plugin = "kanban",
  all = function()
    return boards
  end,
})

Hub.get():entity_snapshot("kanban.board", boards, { owner_plugin = "kanban" })

Hub.get():entity_upsert("kanban.board", {
  id = "board-2",
  name = "Triage",
  status = "active",
}, { owner_plugin = "kanban" })

Hub.get():entity_patch("kanban.board", "board-2", {
  status = "archived",
  counts = { open = 0 },
}, { owner_plugin = "kanban" })

Hub.get():entity_remove("kanban.board", "board-2", { owner_plugin = "kanban" })
```

Lua-authored surfaces bind to these plugin entities with the normal UI
contract binding grammar. The plugin entity type is the first path segment:

```lua
ui.bind("/kanban.board/board-1/name")

ui.bind_list{
  source = "/kanban.board",
  where = { status = "active" },
  item_template = ui.tree_item{
    id = ui.bind("@/id"),
    title = { ui.text{ text = ui.bind("@/name") } },
    subtitle = { ui.text{ text = ui.bind("@/status") } },
  },
}
```

Dynamic plugin lists should use entity-backed `ui.bind_list` rows instead of
forcing a fresh `ui_tree_snapshot` after every field or collection change.
Use `where = { field = value }` to scope a list to records whose top-level
fields match exactly, such as one run's steps or one pipeline step's gates.
The filter is part of the shared browser/TUI UI contract; do not pre-render
filtered child collections into the tree snapshot just to get per-record rows.
`ui_tree_snapshot` remains the presentation tree; plugin state changes flow
through `entity_snapshot`, `entity_upsert`, `entity_patch`, and
`entity_remove`.

## Browser-Local Presentation State

Use `ui.local_state(key, default)` for ephemeral per-client presentation state:
modal open flags, disclosure toggles, focused panes, and similar browser-only
UI affordances. Local state is scoped by hub and surface in the browser. The TUI
resolver treats the binding as its default value so plugin trees remain
cross-client without adding a second model path.

```lua
ui.button{
  text = "Spawn agent",
  action = ui.action("botster.presentation.set", {
    key = "ticket-123-spawn-agent-open",
    value = true,
  }),
}

ui.dialog{
  open = ui.local_state("ticket-123-spawn-agent-open", false),
  title = "Spawn agent",
  children = { ... },
}
```

Plugins can change these values with `botster.presentation.set`,
`botster.presentation.clear`, and `botster.presentation.toggle`. Action results
may also return `presentation = { clear = { "key-a", "key-b" } }` or
`presentation = { set = { { key = "key-a", value = true } } }` to clean up local
state after hub-side work succeeds.

Do not encode modal state in plugin routes, plugin entities, or `plugin.db`.
Those paths represent shared navigation or durable model state and can force
surface reloads or leak one browser's UI state to other clients.

When registering outside plugin load tests or helper modules, pass
`owner_plugin = "kanban"`. During normal plugin loading, Botster supplies the
owner context from the plugin manifest/display name; repo-sourced loader keys
include paths and are not valid wire namespaces. Hot reload may re-register the
same type from the same plugin; another plugin cannot take ownership of that
entity type.

`Hub.get():entity_snapshot`, `entity_upsert`, `entity_patch`, and
`entity_remove` only publish plugin-owned entity types. They reject built-in
types, cross-plugin namespaces, unregistered plugin types, and records whose
`id` is not a non-empty string. `entity_snapshot` is for replacing the client
baseline after a plugin refresh or explicit client request; requested
snapshots come from the registered `all()` function.

Snapshot `all()` callbacks must return an array of entity tables. Records
without a string `id` are logged and skipped. Patches merge only top-level
fields; nested tables replace prior nested values, so send the full nested value
you want clients to keep. Broadcaster errors are logged and isolated from the
mutator path.

Project Pipelines is the reference plugin for this pattern. It registers every
workflow record family in
`catalog/templates/plugins/project-pipelines/project_pipelines/entities.lua`,
publishes targeted recovery baselines through `publish_snapshots()`, and lets
repo mutators call `entities.upsert(...)` or `entities.remove(...)` after
persistence changes. Plugin-owned entity families are not part of the initial
browser/TUI hub baseline; surfaces request the plugin data they need. Its
overview binds lists from `/project-pipelines.ticket`,
`/project-pipelines.project`, and `/project-pipelines.pipeline`; its project
detail screen filters the shared ticket family with
`ui.bind_list{ where = { project_id = project_id } }`. Its web actions return
`action.HANDLED` for local draft changes and `action.result{...}` for
submitters that need generic `ui_action_result` feedback. See
[`../plugin-entities.md`](../plugin-entities.md) for the authoring guide.

## Event-Driven Primitives

### `webrtc`
```lua
webrtc.on_peer_connected(fn(peer_id))
webrtc.on_peer_disconnected(fn(peer_id))
webrtc.on_message(fn(peer_id, msg_table))
webrtc.send(peer_id, table)
webrtc.send_binary(peer_id, data)
webrtc.subscribe_terminal(opts) -- opts: {session_uuid, subscription_id, peer_id, prefix, ...}
```

`subscribe_terminal` is the shared terminal attach primitive for transport
adapters. WebRTC, TUI, and socket adapters use different framing at the edge,
but all three create the same client-worker/session-I/O subscription: the hub
authorizes and correlates attach work, while durable-session PTY bytes,
scrollback, and raw input stay on the client/session actor data plane.

### `tui`
```lua
tui.on_connected(fn())
tui.on_disconnected(fn())
tui.on_message(fn(msg_table))
tui.send(msg)
tui.send_binary(data)
tui.subscribe_terminal(opts)
```

### `socket`
```lua
socket.on_client_connected(fn(client_id))
socket.on_client_disconnected(fn(client_id))
socket.on_message(fn(client_id, msg_table))
socket.send(client_id, msg)
socket.send_binary(client_id, data)
socket.subscribe_terminal(opts)
```

### `pty`
```lua
-- Runtime PTY spawn is broker-authoritative via the hub primitive.
local handle, broker_session_id = hub.spawn_pty_with_broker(config, session_uuid)

-- `pty.spawn(config)` exists only in Rust unit tests as a fixture primitive.
-- Runtime Lua modules should not call it.

handle:write(data)
handle:kill()
handle:resize(rows, cols)
handle:is_alive() -> bool
handle:port() -> number           -- for port-forward sessions
```

### `hub`
```lua
hub.hub_id() -> string             -- local identifier (SHA256 hash, matches hub_discovery IDs)
hub.server_id() -> string          -- server-assigned ID (set after registration)
hub.get_worktrees() -> table
hub.register_agent(key, handles)
hub.unregister_agent(key)
hub.quit()
hub.detect_repo() -> string
hub.handle_signaling_message(message)
hub.prepare_plugin_command({
  request_id = "plugin-owned-token",
  command = "tool-or-/absolute/path",
  config_path = "/tmp/tool.json",       -- optional
  config_contents = "{}\n",             -- optional
  context = { session_uuid = "sess-..." } -- optional, round-tripped
})
-- fires plugin_command_prepared
hub.run_command_gate({
  request_id = "plugin-owned-token",
  command = "bin/check-workflow-ready",
  cwd = "/repo/worktree",
  timeout_secs = 30,
  env = { RAILS_ENV = "test" },          -- optional
  config_path = "/tmp/gate.json",        -- optional
  config_contents = "{}\n",              -- optional
  metadata = { stage = "verify" },       -- optional, round-tripped
  context = { session_uuid = "sess-..." } -- optional, round-tripped
})
-- fires command_gate_completed
hub.probe_url_ready(connector_uuid, parent_uuid, url, hostname, timeout_secs?)
```

`hub.handle_signaling_message(message)` forwards a decrypted ActionCable
signaling/control payload to the Rust hub for routing. Example:

```lua
hub.handle_signaling_message({
    type = "signal",
    browser_identity = browser_identity,
    envelope = { type = "offer", sdp = "v=0 ..." },
})
```

Supported messages:
- `type = "signal"` with `envelope.type = "offer"` or `"ice"`
- `type = "bundle_request"` with `browser_identity`

`hub.prepare_plugin_command(opts)` resolves an executable and optionally writes a
small config file on the blocking worker pool, then emits
`plugin_command_prepared` with:

```lua
{
  request_id = opts.request_id,
  command = "/resolved/executable", -- nil on failure
  config_path = opts.config_path,
  context = opts.context,
  error_kind = nil | "command_blank" | "command_missing" | "config_write_failed" | "task_failed",
  error = nil | "message",
}
```

Use this from plugin action handlers before spawning connector/accessory
processes that need PATH resolution or a generated config file. Keep
plugin-specific behavior in the plugin; the hub primitive is only the generic
blocking-work offload and completion event.

`hub.run_command_gate(opts)` runs a one-shot command on the blocking
worker pool and emits `command_gate_completed` with:

```lua
{
  request_id = opts.request_id,
  metadata = opts.metadata,
  context = opts.context,
  success = true | false,
  exit_status = 0, -- nil when unavailable
  stdout_tail = "...", -- bounded capture
  stderr_tail = "...", -- bounded capture
  output_truncated = false,
  error_kind = nil | "command_blank" | "cwd_missing" | "cwd_invalid" |
    "timeout_invalid" | "command_parse_failed" | "command_missing" |
    "config_write_failed" | "spawn_failed" | "wait_failed" |
    "timeout" | "exit_status" | "task_failed",
  error = nil | "message",
  duration_ms = 123,
}
```

`command`, `cwd`, `request_id`, and `timeout_secs` are required. The primitive
parses `command`, resolves the first word and optional config materialization
through `hub.prepare_plugin_command`'s helper path, then runs the command as a
non-PTY captured process. It rejects blank commands and missing/invalid working
directories before spawning. Captured stdout/stderr tails are bounded so noisy
workflow gates cannot grow hub memory without limit. Store the active
`request_id` in plugin state and ignore stale completions after retries or
reloads.

`metadata` and `context` are trusted-plugin payloads echoed back into the
completion event, not durable storage or client-facing display text. Keep them
small, JSON-serializable, and limited to correlation fields such as
`session_uuid`, workflow stage, attempt number, or plugin request tokens. Do
not put secrets or unbounded command output in either field.

`hub.probe_url_ready(...)` waits asynchronously for public DNS and HTTPS
reachability before a plugin surfaces a public URL to clients.

### `connection`
```lua
connection.generate()              -- triggers connection_code_ready event
connection.regenerate()
connection.copy_to_clipboard()
```

### `worktree`
```lua
worktree.find(branch) -> path
worktree.list() -> table
worktree.create_async(opts)        -- opts: {branch, repo_root?, prompt, metadata?, ...}
worktree.delete(path, branch)
worktree.repo_root() -> string
worktree.is_git_repo() -> bool
worktree.copy_from_patterns(src, dst, patterns_file)
```

### `events`
```lua
local sub_id = events.on(event, fn(data))
events.off(sub_id)
events.emit(event, data)          -- Lua-side emit; Rust also emits into this
```

### `http`
```lua
-- Positional shortcuts (SYNC, BLOCKING - only safe at plugin load time, NOT in callbacks)
http.get(url, headers?) -> {status, body, headers}
http.post(url, body, headers?) -> {status, body, headers}
http.put(url, body, headers?) -> {status, body, headers}
http.delete(url, headers?) -> {status, body, headers}

-- Table-first async form (RECOMMENDED for callbacks and runtime)
http.request({method="POST", url="...", headers={}, body/json=...}, function(resp, err))
  -- resp: {status, body, headers} on success, nil on error
  -- err: error string on failure, nil on success
```

**Calling convention guide:**
- **Table-first** (`http.request({...}, callback)`) — async, non-blocking. Callback runs asynchronously when request completes. Safe everywhere, especially in event handlers and plugin callbacks.
- **Positional** (`http.get(url)`, etc.) — sync, BLOCKING the event loop. Only safe at plugin load time (before the hub event loop starts). Using these in callbacks will stall the hub and break WebRTC health checks.

**Critical gotcha:** Calling `http.get()` inside a callback blocks the entire hub event loop, preventing `dc_pong` responses from reaching connected web clients within the 30-second health check window. This triggers a WebRTC disconnect. See [[HTTP blocking calls break WebRTC health checks]] in the knowledge vault.

### `timer`
```lua
local id = timer.after(seconds, fn())    -- one-shot
local id = timer.every(seconds, fn())    -- repeating
timer.cancel(id)
```

### `watch`
```lua
local id = watch.directory(path, opts?, callback)
-- opts: {
--   recursive = true,       -- watch subdirectories (default: true)
--   pattern = "*.lua",      -- glob filter (optional)
--   poll = false,           -- use mtime polling instead of OS events (default: false)
--   poll_interval = 2.0,    -- poll interval in seconds (default: 2.0)
-- }
-- callback: function(event) where event = {path, kind, watch_id}
-- kind: "create" | "modify" | "rename" | "delete"
watch.unwatch(id) -> bool
```

Use `poll = true` when OS-native watching (FSEvents on macOS) misses in-place file writes. The plugin hot-reload watcher uses this by default.

### `websocket`
```lua
local conn_id = websocket.connect(url, {
    headers    = { ... },   -- optional
    on_open    = fn(),
    on_message = fn(msg),
    on_close   = fn(code, reason),
    on_error   = fn(err)
})
websocket.send(conn_id, msg)    -- module function, NOT conn_id.send()
websocket.close(conn_id)        -- module function, NOT conn_id.close()

-- Both return (true, nil) or (nil, error_string)
```

**Non-blocking:** Spawns connection on dedicated OS thread. Callbacks (`on_open`, `on_message`, etc.) are async and safe.

**Critical gotcha:** `conn_id` is a string ID, not an object. `conn_id.send(msg)` and `conn_id.close()` do NOT exist — use `websocket.send(conn_id, msg)` and `websocket.close(conn_id)` (module functions, not methods).

### `action_cable`
```lua
local conn = action_cable.connect(opts?)   -- opts: {crypto=true, ...} - no URL arg, uses hub's default cable endpoint
local channel_id = action_cable.subscribe(conn, channel_name, params, callback)
action_cable.unsubscribe(channel_id)
action_cable.perform(channel_id, action, data)
action_cable.close(conn)
```

**Non-blocking:** All operations are async. Channel subscriptions and messages route through `HubEvent` channel. Callbacks receive `(msg, channel_id)` asynchronously.

**Key detail:** `action_cable.connect()` takes no URL argument — it auto-connects to the hub's Rails cable endpoint. The `crypto=true` option auto-decrypts signal envelopes for end-to-end encrypted messaging.

### `hub_discovery`
```lua
hub_discovery.list() -> {{id, socket, repo_path}, ...}  -- all running hubs on this machine
hub_discovery.is_running(hub_id) -> bool
hub_discovery.socket_path(hub_id) -> string
```

### `hub_client`
```lua
local conn_id = hub_client.connect(socket_path)    -- connect to another hub's socket
hub_client.on_message(conn_id, fn(message, conn_id))
hub_client.send(conn_id, table)                    -- send JSON message
hub_client.close(conn_id)
```

### `mcp` (Lua module, not a Rust primitive)

Lua-side MCP tool registry. Plugins register tools that agents receive through
the marketplace-installed Botster tooling.

```lua
-- Register a tool (typically in a plugin's init.lua)
mcp.tool("my_tool", {
    description = "What this tool does",
    input_schema = {
        type = "object",
        properties = {
            arg1 = { type = "string", description = "..." },
        },
        required = { "arg1" },
    },
}, function(params, context)
    -- params: the arguments from the MCP client
    -- context: { session_uuid, hub_id } injected by the hub
    return "result string"           -- or return a table (auto JSON-encoded)
end)

-- Other API
mcp.remove_tool(name)
mcp.reset(source)                    -- clear tools by source (used during hot-reload)
mcp.list_tools() -> table            -- metadata only, no handlers
mcp.call_tool(name, params, context) -> result, error
mcp.count() -> number
```

Tools track their source plugin automatically via `_G._loading_plugin_source` (set by `loader.lua`). On plugin hot-reload, `mcp.reset(source)` clears that plugin's tools before re-registering. The hub emits a `tools_list_changed` notification to connected MCP clients so they re-fetch the tool list.

**Agent tool installation**: Plugin-provided tools are installed for agents by
the external plugin marketplace. Do not add Botster tool servers from session
initialization scripts. Session definitions should focus on launching the agent
process and should use `botster context ...` helpers for runtime context.

### `update`
```lua
update.check() -> {available, version, ...}
update.install() -> {success, error, ...}
```

### `push`
```lua
push.send({title, body, url, ...})    -- web push notifications
```

### `lib.notifications`
```lua
local notifications = require("lib.notifications")

notifications.observe({
  name = "plugin.audit",
  scope = { session_uuid = "sess-..." }, -- also supports sessions, owner_plugin, surface, all_sessions
  -- all_sessions requires capabilities = { "notifications.global_observe" }
  phase = "both",                        -- "before", "after", or "both"
  handler = function(phase, intent, decision) end,
})

notifications.claim({
  name = "plugin.owner",
  scope = { owner_plugin = "plugin" },
  -- all_sessions requires capabilities = { "notifications.global_claim" }
  handler = function(intent)
    return {
      core = "default", -- "default", "suppress", or "replace"
      reason = "optional_debug_reason",
      custom = {
        title = "Custom title",
        body = intent.message,
        push = true,
        transient = true,
        badge = true,
      },
    }
  end,
})
```

Observers never change delivery. Claims provide one effective owner decision for
matching notifications. Plugin-owned handlers execute in the plugin worker; the
hub owns session badge mutation, web push, transient UI events, and fallback to
default behavior on handler errors/timeouts.
