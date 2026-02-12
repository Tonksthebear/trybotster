-- ui/botster.lua — Neovim-inspired API for TUI extensions.
--
-- Loaded into the TUI Lua state after built-in modules (layout, keybindings,
-- actions) and before plugin/user extensions. Provides a clean API surface
-- so extensions don't need to poke at raw globals.
--
-- Usage in user/plugin extensions:
--   botster.keymap.set("normal", "ctrl+n", "my_action", { desc = "..." })
--   botster.action.register("my_action", function(ctx) ... end)
--   botster.ui.register_component("status", function(state) ... end)

-- Preserve botster.g across hot-reloads (if botster already exists, keep .g)
local prev_g = (type(botster) == "table" and botster.g) or {}

botster = {}
botster.g = prev_g

-- =============================================================================
-- Config Utilities
-- =============================================================================

--- Deep merge tables, matching vim.tbl_deep_extend semantics.
-- @param behavior string "force" (later wins) or "keep" (earlier wins)
-- @param ... tables to merge
-- @return table merged result
function botster.tbl_deep_extend(behavior, ...)
  local result = {}
  for i = 1, select("#", ...) do
    local tbl = select(i, ...)
    if type(tbl) == "table" then
      for k, v in pairs(tbl) do
        if type(v) == "table" and type(result[k]) == "table" and not v[1] then
          -- Recursively merge dict-like tables (skip arrays)
          result[k] = botster.tbl_deep_extend(behavior, result[k], v)
        elseif behavior == "force" or result[k] == nil then
          result[k] = v
        end
      end
    end
  end
  return result
end

-- =============================================================================
-- Keymap API
-- =============================================================================

botster.keymap = {}

-- Registry: namespace -> { { mode, key, action, desc } }
local keymap_registry = {}

-- Function-based keybinding storage: key descriptor -> function
-- Used when action is a function instead of a string
local keymap_functions = {}

--- Register a keybinding.
-- @param modes string|table Mode name(s) (e.g., "normal" or {"normal", "menu"})
-- @param key string Key descriptor (e.g., "ctrl+n", "shift+enter")
-- @param action string|function Action name or function(context) -> {action=...}
-- @param opts table|nil { desc = "...", namespace = "default" }
function botster.keymap.set(modes, key, action, opts)
  opts = opts or {}
  local ns = opts.namespace or "default"
  local desc = opts.desc or ""

  if type(modes) == "string" then
    modes = { modes }
  end

  keymap_registry[ns] = keymap_registry[ns] or {}

  for _, mode in ipairs(modes) do
    -- Store in registry for listing/clearing
    table.insert(keymap_registry[ns], {
      mode = mode,
      key = key,
      action = action,
      desc = desc,
    })

    -- Apply to _keybindings tables
    if _keybindings then
      _keybindings[mode] = _keybindings[mode] or {}
      if type(action) == "function" then
        -- Store function, use a sentinel action name
        local fn_key = mode .. ":" .. key
        keymap_functions[fn_key] = action
        _keybindings[mode][key] = "__fn:" .. fn_key
      else
        _keybindings[mode][key] = action
      end
    end
  end
end

--- Remove a keybinding.
-- @param modes string|table Mode name(s)
-- @param key string Key descriptor
-- @param opts table|nil { namespace = "..." } (removes from specific namespace)
function botster.keymap.del(modes, key, opts)
  opts = opts or {}

  if type(modes) == "string" then
    modes = { modes }
  end

  for _, mode in ipairs(modes) do
    -- Remove from _keybindings
    if _keybindings and _keybindings[mode] then
      local fn_key = mode .. ":" .. key
      keymap_functions[fn_key] = nil
      _keybindings[mode][key] = nil
    end

    -- Remove from registry
    local ns = opts.namespace
    if ns then
      local entries = keymap_registry[ns]
      if entries then
        for i = #entries, 1, -1 do
          if entries[i].mode == mode and entries[i].key == key then
            table.remove(entries, i)
          end
        end
      end
    else
      -- Remove from all namespaces
      for _, entries in pairs(keymap_registry) do
        for i = #entries, 1, -1 do
          if entries[i].mode == mode and entries[i].key == key then
            table.remove(entries, i)
          end
        end
      end
    end
  end
end

--- List all registered keybindings.
-- @param opts table|nil { namespace = "..." } to filter, { mode = "..." } to filter
-- @return table Array of { mode, key, action, desc, namespace }
function botster.keymap.list(opts)
  opts = opts or {}
  local result = {}

  for ns, entries in pairs(keymap_registry) do
    if not opts.namespace or opts.namespace == ns then
      for _, entry in ipairs(entries) do
        if not opts.mode or opts.mode == entry.mode then
          table.insert(result, {
            mode = entry.mode,
            key = entry.key,
            action = entry.action,
            desc = entry.desc,
            namespace = ns,
          })
        end
      end
    end
  end

  return result
end

--- Remove all keybindings from a namespace.
-- @param namespace string Namespace to clear
function botster.keymap.clear_namespace(namespace)
  local entries = keymap_registry[namespace]
  if not entries then return end

  for _, entry in ipairs(entries) do
    if _keybindings and _keybindings[entry.mode] then
      local fn_key = entry.mode .. ":" .. entry.key
      keymap_functions[fn_key] = nil
      _keybindings[entry.mode][entry.key] = nil
    end
  end

  keymap_registry[namespace] = nil
end

--- Resolve a function-based keybinding. Called from handle_key wrapper.
-- @param fn_key string The "__fn:mode:key" action string
-- @param context table Key context
-- @return table|nil Action table or nil
function botster.keymap._resolve_fn(fn_key, context)
  local key = fn_key:sub(6)  -- strip "__fn:" prefix
  local fn = keymap_functions[key]
  if fn then
    return fn(context)
  end
  return nil
end

-- =============================================================================
-- Action API
-- =============================================================================

botster.action = {}

-- Registry: name -> { callback, namespace, desc }
local action_handlers = {}

--- Register a named action handler.
-- @param name string Action name
-- @param callback function(context) -> ops_table|nil
-- @param opts table|nil { desc = "...", namespace = "default" }
function botster.action.register(name, callback, opts)
  opts = opts or {}
  action_handlers[name] = {
    callback = callback,
    namespace = opts.namespace or "default",
    desc = opts.desc or "",
  }
end

--- Remove a registered action handler.
-- @param name string Action name
function botster.action.unregister(name)
  action_handlers[name] = nil
end

--- List all registered action handlers.
-- @return table Array of { name, desc, namespace }
function botster.action.list()
  local result = {}
  for name, handler in pairs(action_handlers) do
    table.insert(result, {
      name = name,
      desc = handler.desc,
      namespace = handler.namespace,
    })
  end
  table.sort(result, function(a, b) return a.name < b.name end)
  return result
end

--- Resolve an action. Returns ops from custom handler, or nil to fall through.
-- @param name string Action name
-- @param context table Action context
-- @return table|nil Ops list or nil
function botster.action._resolve(name, context)
  local handler = action_handlers[name]
  if handler then
    local ok, result = pcall(handler.callback, context)
    if ok then
      return result
    end
    -- Log error but don't crash
    -- (no log global in TUI Lua state, so print to stderr)
  end
  return nil
end

-- =============================================================================
-- UI Component API
-- =============================================================================

botster.ui = {}

-- Registry: name -> function(state) -> styled_span_or_render_node
local component_registry = {}

--- Register a named UI component.
-- @param name string Component name
-- @param render_fn function(state) -> render node or styled span
function botster.ui.register_component(name, render_fn)
  component_registry[name] = render_fn
end

--- Get a registered component's render function.
-- @param name string Component name
-- @return function|nil The render function, or nil if not registered
function botster.ui.get_component(name)
  return component_registry[name]
end

--- Render a component by name.
-- @param name string Component name
-- @param state table Render state
-- @return any Render output, or nil if component not found
function botster.ui.render_component(name, state)
  local fn = component_registry[name]
  if fn then
    local ok, result = pcall(fn, state)
    if ok then return result end
  end
  return nil
end

--- List all registered components.
-- @return table Array of component names
function botster.ui.list_components()
  local result = {}
  for name in pairs(component_registry) do
    table.insert(result, name)
  end
  table.sort(result)
  return result
end

-- =============================================================================
-- Internal: Wire up action and keymap dispatch
-- =============================================================================

--- Wire botster action handlers into _actions.on_action dispatch.
-- Called after all extensions are loaded. Safe to call multiple times
-- (uses _original_on_action to avoid recursive wrapping).
function botster._wire_actions()
  if not _actions then return end

  -- Capture the unwrapped original once, reuse on subsequent calls
  if not botster._original_on_action then
    botster._original_on_action = _actions.on_action
  end

  local original = botster._original_on_action

  _actions.on_action = function(action, context)
    -- Check botster-registered actions first
    local result = botster.action._resolve(action, context)
    if result then return result end

    -- Fall through to built-in actions
    if original then
      return original(action, context)
    end

    return nil
  end
end

--- Wire botster function keybindings into _keybindings.handle_key dispatch.
-- Called after all extensions are loaded. Safe to call multiple times
-- (uses _original_handle_key to avoid recursive wrapping).
function botster._wire_keybindings()
  if not _keybindings then return end

  -- Capture the unwrapped original once, reuse on subsequent calls
  if not botster._original_handle_key then
    botster._original_handle_key = _keybindings.handle_key
  end

  local original = botster._original_handle_key

  _keybindings.handle_key = function(key, mode, context)
    -- Check built-in bindings first (including botster.keymap.set ones)
    local result
    if original then
      result = original(key, mode, context)
    end

    -- If result is a function-based binding, resolve it
    if result and type(result) == "table" and result.action then
      local action = result.action
      if type(action) == "string" and action:sub(1, 5) == "__fn:" then
        return botster.keymap._resolve_fn(action, context)
      end
    end

    return result
  end
end
