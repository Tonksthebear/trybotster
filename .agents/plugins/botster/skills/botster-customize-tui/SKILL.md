---
name: botster-customize-tui
description: Use when customizing the Botster TUI layout, keybindings, actions, overlays, or reading TUI agent state.
---

# Botster Customize TUI

TUI customizations live in the active Botster config directory:

- `lua/user/ui/layout.lua` replaces or wraps the main layout.
- `lua/user/ui/keybindings.lua` adds or rebinds keys.
- `lua/user/ui/actions.lua` defines custom action handlers.

Debug builds use `~/.botster-dev/`; release builds use `~/.botster/`.
Repo-specific config may live under `.botster/` in the spawn target.

## Layout

Rust calls these globals every frame:

```lua
render(state)
render_overlay(state)
```

Wrap the built-in layout when making small changes:

```lua
local original_render = render

render = function(state)
  local tree = original_render(state)
  return tree
end
```

Replace the layout when the structure changes substantially:

```lua
render = function(state)
  return {
    type = "hsplit",
    constraints = { "25%", "75%" },
    children = {
      { type = "list", props = { items = {}, selected = 0 } },
      { type = "terminal", props = { session_uuid = _tui_state.selected_session_uuid } },
    },
  }
end
```

## State

Read `_tui_state` inside render functions. Important fields:

- `agents`
- `selected_session_uuid`
- `list_cursor_pos`
- `list_selected`
- `available_worktrees`
- `available_profiles`
- `pending_fields`
- `mode`
- `input_buffer`

Use `session_uuid` for routing. Do not use older `agent_key` vocabulary.

## Keybindings

```lua
local kb = require("ui.keybindings")

kb.normal["ctrl+h"] = "show_connection_code"
kb.insert["ctrl+h"] = "show_connection_code"
```

Key names include `enter`, `ctrl+p`, `shift+enter`, and `ctrl+]`. `ctrl+q` is
handled by Rust and does not reach Lua.

## Built-In Modes

- `normal`
- `insert`
- `menu`
- `new_agent_select_profile`
- `new_agent_select_worktree`
- `new_agent_create_worktree`
- `new_agent_prompt`
- `close_agent_confirm`
- `connection_code`
- `error`

## Shared Bindings

- `ctrl+p` — `open_menu`
- `ctrl+j` — `select_next`
- `ctrl+k` — `select_previous`
- `ctrl+]` — `toggle_pty`
- `shift+pageup` — `scroll_half_up`
- `shift+pagedown` — `scroll_half_down`
- `shift+home` — `scroll_top`
- `shift+end` — `scroll_bottom`
- `ctrl+r` — `refresh_agents`

## Render Node Types

- `hsplit`
- `vsplit`
- `centered`
- `list`
- `paragraph`
- `input`
- `terminal`
- `connection_code`
- `empty`

## Action Ops

- `set_mode`
- `send_msg`
- `focus_terminal`
- `quit`

## TUI Extension API

- `botster.keymap.set(modes, key, action_or_fn, opts)`
- `botster.keymap.del(modes, key, opts)`
- `botster.keymap.list(opts)`
- `botster.keymap.clear_namespace(ns)`
- `botster.action.register(name, fn, opts)`
- `botster.action.unregister(name)`
- `botster.action.list()`
- `botster.ui.register_component(name, fn)`
- `botster.ui.get_component(name)`
- `botster.ui.render_component(name, state)`
- `botster.ui.list_components()`
- `botster.tbl_deep_extend(behavior, ...)`
- `botster.g`

## Gotchas

Create new customization files before hub startup, then restart once. After the
watcher sees the file/directory, later saves hot-reload.

## References

- `docs/lua/tui-configuration.md` — complete TUI modes, nodes, ops, and extension API.
- `docs/lua/directory-structure.md` — config file discovery.
- `docs/lua/hot-reload.md` — reload behavior.
