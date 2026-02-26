# Botster Hot Reload and Plugin Loading

## Protected vs Hot-Reloadable Modules

Three modules are protected — never reloaded, their state survives:

- `hub.state` — KV store for hot-reload persistence
- `hub.hooks` — observer/interceptor registry
- `hub.loader` — module reloader + sandbox executor

Everything else (all `lib/`, `handlers/`, `ui/` modules) is hot-reloadable.

## Hot-Reload Lifecycle Protocol

Hot-reloadable modules implement optional lifecycle callbacks:

```lua
function M._before_reload()   -- called before unload (cleanup subscriptions, hooks)
    events.off(sub_id)
    hooks.off("event_name", "hook_name")
end

function M._after_reload()    -- called after successful reload (logging, assertions)
end
```

State that must survive reloads uses `hub.state`:

```lua
local my_data = state.get("my_plugin_data") or {}
-- or for class-like tables:
local MyClass = state.class("MyClass")
```

## Hub-Side Hot-Reload

1. `notify` crate filesystem watcher watches `~/.botster/lua/` recursively
2. `spawn_blocking` forwarder converts `notify::Event` to module names via `path_to_module()` (path separators -> dots, strips `.lua`)
3. Forwarder sends `HubEvent::LuaFileChange { modules: Vec<String> }` to the hub event loop
4. Hub calls `LuaRuntime::reload_lua_modules()` -> `reload_module(name)` -> `loader.reload(name)` in Lua
5. `loader.reload()`:
   - Checks `protected_modules` table (blocks hub.state, hub.hooks, hub.loader)
   - Calls `old_module._before_reload()` if it exists
   - Sets `package.loaded[name] = nil` (forces re-require)
   - Calls `require(name)` again
   - Calls `new_module._after_reload()` if it exists
   - On failure: restores old module, logs error

Trigger conditions: `Create`, `Modify`, `Rename` file events on `.lua` files only. Deletes are ignored.

## TUI-Side Hot-Reload

1. `HotReloader` in `src/tui/hot_reload.rs` watches:
   - Source tree `lua/ui/` (debug builds only)
   - `~/.botster/lua/ui/` and `~/.botster/lua/user/ui/`
   - Plugin `ui/` directories
2. On any `.lua` change, `HotReloader::poll()` (called each tick) re-reads changed files and calls the appropriate `LayoutLua` method:
   - `reload()` for `layout.lua`
   - `reload_keybindings()` for `keybindings.lua`
   - `reload_actions()` for `actions.lua`
   - `reload_events()` for `events.lua`
3. Extensions are always replayed after any reload (`replay_extensions()`): re-discovers plugins, reloads botster API, re-wires action/keymap dispatch
4. `_tui_state` global is preserved across reloads (initialized once with `or` guard)

## Plugin File Watcher (Hot-Reload for Plugins)

Core modules (`lib/`, `handlers/`) are watched by the Rust `LuaFileWatcher` (FSEvents/inotify). Plugins in `.botster/` directories use a separate Lua-based watcher (`handlers/plugin_watcher.lua`) that uses `watch.directory()` with `poll = true`.

**Why PollWatcher?** macOS FSEvents misses in-place file writes (the kind agent tools like Claude Code's Edit produce). PollWatcher checks mtimes every 2 seconds, reliably detecting all changes.

The plugin watcher watches the same 4 layers that `ConfigResolver` scans:
1. `~/.botster/shared/plugins/`
2. `~/.botster/profiles/{profile}/plugins/`
3. `{repo}/.botster/shared/plugins/`
4. `{repo}/.botster/profiles/{profile}/plugins/`

On file change:
1. Debounce (0.2s) to avoid rapid-fire reloads from editors
2. If the plugin is already loaded: `loader.reload_plugin(name)` (which calls `mcp.reset(source)` to clear MCP tools, then re-executes the plugin)
3. If it's a new `init.lua`: `loader.load_plugin()` to hot-load a new plugin

MCP tools re-register automatically on reload, and the hub sends a `tools_list_changed` notification to connected MCP clients.

## Plugin Loading

### Loading Order (`hub/init.lua`)

1. Protected core (`hub.state`, `hub.hooks`, `hub.loader`)
2. Library modules: `lib.config_resolver`, `lib.agent`, `lib.commands`
3. Handler modules: all `handlers/*`
4. `events.on("shutdown", ...)` handler
5. `safe_require("user.init")` — user entry point
6. Plugin discovery via `ConfigResolver.resolve_all()` across 4 layers
7. Each plugin loaded via `loader.load_plugin(init_path, name)`
8. Agent improvements from `~/.botster/lua/improvements/*.lua` (sandboxed)

### `loader.load_plugin()` (User Trust Level)

- Reads file via `fs.read(path)`, compiles with `load()`, executes with full `_ENV`
- Registered as `plugin.{name}` in `package.loaded`
- Full access to all Lua primitives — same trust as user code

### Plugin Directory Structure

```
{layer}/plugins/{name}/
  init.lua              # Plugin entry point (hub-side)
  ui/
    layout.lua          # TUI layout override (optional)
    keybindings.lua     # TUI keybindings (optional)
    actions.lua         # TUI actions (optional)
```

Plugins can have both hub-side and TUI-side components.

### `loader.load_improvements()` (Agent Trust Level — Sandboxed)

Agent-written Lua in `~/.botster/lua/improvements/*.lua` runs in a restricted sandbox:

**Allowed:**
- `log`, `hooks`, `events`, `json`, `timer`
- `hub.get_worktrees` (read-only)
- `config.get/all/lua_path/data_dir` (read-only)
- Restricted `fs` (improvements dir only)
- Standard Lua builtins (string, table, math, etc.)

**Blocked:**
- `pty`, `webrtc`, `tui`, `worktree`, `http`, `websocket`, `action_cable`
- `io`, `os.execute`, `debug`, `loadfile`, `dofile`, `require`
- Bytecode loading (text-only `load()` mode)
- Path traversal (confined to improvements directory)

## Rails-Side Templates

Template Lua files live in `app/templates/` and are installed via the `template:install` command:

| Template | Installs to |
|----------|-------------|
| `initialization/basic.lua` | `user/init.lua` — user's personal init |
| `sessions/example.lua` | `sessions/example/init.lua` — starter session config |
| `plugins/github.lua` | `shared/plugins/github/init.lua` — GitHub integration |
