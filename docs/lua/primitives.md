# Botster Lua Primitives Reference

21 Rust modules are registered as Lua globals, plus `mcp` which is a pure Lua module. Core primitives load unconditionally; event-driven primitives require a HubEventSender.

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
```

### `secrets`
```lua
secrets.get(key) -> string       -- plugin-scoped AES-GCM encrypted storage
secrets.set(key, val)
secrets.delete(key)
```

## Event-Driven Primitives

### `webrtc`
```lua
webrtc.on_peer_connected(fn(peer_id))
webrtc.on_peer_disconnected(fn(peer_id))
webrtc.on_message(fn(peer_id, msg_table))
webrtc.send(peer_id, table)
webrtc.send_binary(peer_id, data)
webrtc.create_pty_forwarder(opts) -- opts: {agent_index, pty_index, peer_id, ...}
```

### `tui`
```lua
tui.on_connected(fn())
tui.on_disconnected(fn())
tui.on_message(fn(msg_table))
tui.send(msg)
tui.send_binary(data)
tui.create_pty_forwarder(opts)
```

### `socket`
```lua
socket.on_client_connected(fn(client_id))
socket.on_client_disconnected(fn(client_id))
socket.on_message(fn(client_id, msg_table))
socket.send(client_id, msg)
socket.send_binary(client_id, data)
socket.create_pty_forwarder(opts)
```

### `pty`
```lua
local handle = pty.spawn(config)  -- config: {cmd, args, env, rows, cols, ...}
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
hub.handle_webrtc_offer(identity, sdp)
hub.handle_ice_candidate(identity, candidate)
hub.request_ratchet_restart(identity)
```

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
worktree.create_async(opts)        -- opts: {branch, issue_number, prompt, ...}
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
http.get(url, headers?) -> {status, body, headers}
http.post(url, body, headers?) -> {status, body, headers}
http.put(url, body, headers?) -> {status, body, headers}
http.delete(url, headers?) -> {status, body, headers}
http.request(opts, callback)       -- async: opts = {method, url, body, headers}
```

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
    on_open = fn(),
    on_message = fn(msg),
    on_close = fn(),
    on_error = fn(err)
})
websocket.send(conn_id, msg)
websocket.close(conn_id)
```

### `action_cable`
```lua
local conn = action_cable.connect(opts?)   -- opts: {url, token, ...}
local channel_id = action_cable.subscribe(conn, channel, params, callback)
action_cable.unsubscribe(channel_id)
action_cable.perform(channel_id, action, data)
```

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

Lua-side MCP tool registry. Plugins register tools that agents can invoke via the MCP stdio bridge (`botster mcp-serve`).

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
    -- context: { agent_key, hub_id } injected by the hub
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

**MCP stdio bridge**: Run `botster mcp-serve --socket /path/to/hub.sock` to expose registered tools over JSON-RPC stdio. This is how Claude Code agents call hub tools — configure it as an MCP server in `.mcp.json`:

```json
{
  "mcpServers": {
    "botster": {
      "command": "botster",
      "args": ["mcp-serve", "--socket", "/path/to/hub.sock"]
    }
  }
}
```

### `update`
```lua
update.check() -> {available, version, ...}
update.install() -> {success, error, ...}
```

### `push`
```lua
push.send({title, body, url, ...})    -- web push notifications
```
