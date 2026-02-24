# Botster TUI Lua Configuration

The TUI runs a separate `mlua::Lua` instance (`LayoutLua`) from the hub. Four Lua files drive all TUI behavior, plus `botster.lua` for the plugin extension API. All are hot-reloadable.

## `ui/keybindings.lua`

Called per keypress via `handle_key(key, mode, context)`. Returns an action string or nil.

### Modes

| Mode | Active when | Behavior |
|------|-------------|----------|
| `normal` | No agent selected | Shared modifier bindings only |
| `insert` | Agent selected, PTY active | Shared bindings + unbound keys forward to PTY |
| `menu` | Ctrl+P pressed | Escape/q=close, arrows/j/k=navigate, Enter/Space=select, 1-9=shortcut |
| `new_agent_select_profile` | New agent, multiple profiles | List navigation |
| `new_agent_select_worktree` | Profile selected | List navigation |
| `new_agent_create_worktree` | Create worktree selected | Text input mode |
| `new_agent_prompt` | Worktree selected | Text input mode |
| `close_agent_confirm` | Close menu item | y=close, d=close+delete, n/Esc=cancel |
| `connection_code` | Show connection code | r=regenerate, c=copy, Esc=close |
| `error` | Error occurred | Esc/Enter=dismiss |

### Shared Bindings (active in `normal` + `insert`)

| Key | Action |
|-----|--------|
| `ctrl+p` | `open_menu` |
| `ctrl+j` | `select_next` |
| `ctrl+k` | `select_previous` |
| `ctrl+]` | `toggle_pty` |
| `shift+pageup` | `scroll_half_up` |
| `shift+pagedown` | `scroll_half_down` |
| `shift+home` | `scroll_top` |
| `shift+end` | `scroll_bottom` |
| `ctrl+r` | `refresh_agents` |

`Ctrl+Q` is hardcoded in Rust and never reaches Lua.

## `ui/layout.lua`

Called each frame via `render(state)` and `render_overlay(state)`. Returns a tree of render nodes.

### State Fields (passed from Rust)

| Field | Type | Description |
|-------|------|-------------|
| `state.seconds_since_poll` | number | Controls poll indicator character |
| `state.is_scrolled` | bool | Scrollback indicator |
| `state.scroll_offset` | number | Current scroll position |
| `state.terminal_rows` | number | Terminal dimensions |
| `state.terminal_cols` | number | Terminal dimensions |
| `state.qr_width` | number | QR modal sizing |
| `state.qr_height` | number | QR modal sizing |
| `state.error_message` | string? | For error modal |

### `_tui_state` Global (persists across hot-reloads)

| Field | Description |
|-------|-------------|
| `agents` | Cached agent list |
| `selected_agent_index` | 0-based index |
| `active_pty_index` | Which session is focused |
| `mode` | Current UI mode string |
| `input_buffer` | Current text input |
| `list_selected` | 0-based list cursor |
| `pending_fields` | Wizard state (creating_agent_id, creating_agent_stage, profile, etc.) |
| `available_worktrees` | Worktree list for modal |
| `available_profiles` | Profile list for modal |

### Render Node Types

| Type | Properties | Description |
|------|-----------|-------------|
| `hsplit` | `constraints`, `children` | Horizontal split |
| `vsplit` | `constraints`, `children` | Vertical split |
| `centered` | `width`, `height` (percentages), `child` | Overlay modal |
| `list` | `items` (each: `{text, secondary?, style?, action?, header?}`) | Selectable list |
| `paragraph` | `lines`, `alignment?`, `wrap?` | Static text |
| `input` | `lines` (prompt), `placeholder` | Text input |
| `terminal` | `props: {agent_index, pty_index}` | PTY panel |
| `connection_code` | (special) | QR code display |
| `empty` | (border/title only) | Spacer/placeholder |

Constraints: `"30%"`, `"20"` (fixed), `"min:10"`, `"max:80"`.

## `ui/actions.lua`

Called via `on_action(action, context)`. Returns a table of ops for Rust to execute.

### Ops Returned to Rust

| Op | Fields | Effect |
|----|--------|--------|
| `set_mode` | `mode` | Update Rust's mode shadow |
| `send_msg` | `data: {subscriptionId, data: {type, ...}}` | Send JSON to hub |
| `focus_terminal` | `agent_id, pty_index, agent_index` | Focus a PTY panel |
| `quit` | — | Application exit |

### Context Fields

| Field | Description |
|-------|-------------|
| `context.overlay_actions` | Action strings from the rendered list |
| `context.selected_agent` | Currently selected agent ID |
| `context.terminal_focused` | OS-level window focus |

## `ui/events.lua`

Called via `on_hub_event(event_type, event_data, context)`. Returns ops like actions.

### Additional Ops

| Op | Fields | Effect |
|----|--------|--------|
| `set_connection_code` | `url, qr_ascii` | Store QR data |
| `clear_connection_code` | — | Remove QR data |
| `osc_alert` | `title, body` | Write OSC 777/9 to outer terminal |

### Hub Event Types Handled

`agent_created`, `agent_deleted`, `agent_status_changed`, `agent_list`, `pty_notification`, `worktree_list`, `profiles`, `connection_code`, `connection_code_error`

## `ui/botster.lua` — Extension API

Neovim-inspired API for TUI plugins. Available as the `botster` global.

### Keybinding API
```lua
botster.keymap.set(modes, key, action_or_fn, { desc, namespace })
botster.keymap.del(modes, key, opts)
botster.keymap.list(opts)
botster.keymap.clear_namespace(ns)
```

### Custom Actions
```lua
botster.action.register(name, fn(context) -> ops, { desc, namespace })
botster.action.unregister(name)
botster.action.list()
```

### UI Components
```lua
botster.ui.register_component(name, fn(state) -> render_node)
botster.ui.get_component(name)
botster.ui.render_component(name, state)
botster.ui.list_components()
```

### Utilities
```lua
botster.tbl_deep_extend(behavior, ...)  -- vim.tbl_deep_extend equivalent
botster.g                               -- global state table, persists across hot-reloads
```

`botster._wire_actions()` and `botster._wire_keybindings()` wrap the base action/keybinding dispatch to inject plugin handlers. Extensions are replayed after any hot-reload.
