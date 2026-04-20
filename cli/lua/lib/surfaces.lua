-- Surface registry (Phase 4a) — the Lua-side source of truth for browser
-- surfaces. A "surface" is the unit the hub composes and ships to browsers:
--   * it has a stable `name` (the `target_surface` on the wire)
--   * a `render(state)` function returning a UiNodeV1 table
--   * optional `path` making it a routable browser page
--   * optional `input_builder(client, sub_id)` for per-subscription state
--
-- Plugins call `surfaces.register(...)` from their own Lua init and the
-- registration flows through three places automatically:
--
--   1. `web_layout.render(name, state)` (Rust primitive) falls back to
--      `_G.surfaces.render_node(name, state)` when `layout_table[name]` is not
--      a function — so override files (.botster/layout_web.lua) still win
--      when they define the same name, but otherwise the Lua registry wins.
--   2. `lib.layout_broadcast` iterates `surfaces.list()` when building
--      per-subscription frame lists — every registered surface fans out.
--   3. `handlers.connections` broadcasts `ui_route_registry_v1` on every
--      `surfaces_changed` hook firing, so browsers can discover new routes
--      without a Rails change.
--
-- Storage lives in `hub.state` so hot-reload (Lua module reload, `plugin:reload`)
-- preserves the registry across the reload cycle; the module body is
-- re-evaluated but the underlying table identity is reused.

local state = require("hub.state")

local M = {}

-- Storage shape:
--   registry.by_name[name] = {
--       name, path, label, icon, render, input_builder,
--       hide_from_nav, order, source,
--   }
--   registry.seq — monotonically increasing insertion counter for stable
--     secondary sort when two surfaces share an `order`.
local registry = state.get("surfaces.registry", { by_name = {}, seq = 0 })
if registry.by_name == nil then registry.by_name = {} end
if registry.seq == nil then registry.seq = 0 end

-- -------------------------------------------------------------------------
-- Internal helpers
-- -------------------------------------------------------------------------

local function is_nonempty_string(v)
    return type(v) == "string" and v ~= ""
end

local function notify_changed()
    if type(hooks) == "table" and type(hooks.notify) == "function" then
        pcall(hooks.notify, "surfaces_changed", { registry = M })
    end
end

-- -------------------------------------------------------------------------
-- Public API
-- -------------------------------------------------------------------------

--- Register (or replace) a surface.
--
-- Replacing an existing registration is intentional — plugin reloads re-call
-- `register(...)` on the same name, and the registry must end up with the
-- fresh closure rather than a stale one captured before the reload.
--
-- @param name string — wire `target_surface` identifier.
-- @param opts table — see module doc.
-- @return the stored entry table (for chaining / tests).
function M.register(name, opts)
    assert(is_nonempty_string(name), "surfaces.register: name must be non-empty string")
    assert(type(opts) == "table", "surfaces.register: opts must be a table")
    assert(type(opts.render) == "function", "surfaces.register: opts.render must be a function")

    local existing = registry.by_name[name]
    local seq = existing and existing.seq or (registry.seq + 1)
    if not existing then registry.seq = seq end

    local entry = {
        name = name,
        path = is_nonempty_string(opts.path) and opts.path or nil,
        label = is_nonempty_string(opts.label) and opts.label or name,
        icon = is_nonempty_string(opts.icon) and opts.icon or nil,
        render = opts.render,
        input_builder = type(opts.input_builder) == "function" and opts.input_builder or nil,
        hide_from_nav = opts.hide_from_nav == true,
        order = type(opts.order) == "number" and opts.order or nil,
        source = is_nonempty_string(opts.source) and opts.source or nil,
        seq = seq,
    }
    registry.by_name[name] = entry

    if log and log.debug then
        log.debug(string.format(
            "surfaces.register: name=%s path=%s label=%s",
            name, tostring(entry.path), tostring(entry.label)))
    end
    notify_changed()
    return entry
end

--- Remove a previously registered surface.
-- @param name string
-- @return true if the entry existed and was removed, false otherwise.
function M.unregister(name)
    if not is_nonempty_string(name) then return false end
    if registry.by_name[name] == nil then return false end
    registry.by_name[name] = nil
    if log and log.debug then
        log.debug(string.format("surfaces.unregister: name=%s", name))
    end
    -- Purge the broadcast module's per-subscription dedup baselines for
    -- this surface. Without this, versions_by_key[sub_id][name] would
    -- accumulate forever (one entry per subscription that ever saw the
    -- old tree), AND a re-registration of the same name could be
    -- silently swallowed by dedup if the new tree's hash matched the
    -- stale one. Synchronous so the next render in this tick already
    -- sees a clean baseline. pcall in case lib.layout_broadcast isn't
    -- loaded (test harnesses that only import surfaces.lua).
    local ok_broadcast, broadcast = pcall(require, "lib.layout_broadcast")
    if ok_broadcast and type(broadcast) == "table"
        and type(broadcast.forget_surface) == "function"
    then
        pcall(broadcast.forget_surface, name)
    end
    notify_changed()
    return true
end

--- Look up a registered surface.
-- @return the entry table (render fn included) or nil.
function M.get(name)
    if not is_nonempty_string(name) then return nil end
    return registry.by_name[name]
end

--- Resolve a surface name to its declared path.
-- @return string path, or nil when the surface is non-routable (no `path`).
function M.path(name)
    local entry = M.get(name)
    return entry and entry.path or nil
end

--- Return a deterministic array view of the registry.
--
-- Sort key: (order asc, seq asc, name asc) — surfaces with an explicit
-- `order` sort first (lower wins), then registration order, then name. This
-- keeps the sidebar nav stable across reloads.
--
-- Each returned entry is a shallow copy EXCLUDING the render/input_builder
-- closures — consumers that need those call `surfaces.get(name)` directly.
-- Stripping closures from `list()` keeps it JSON-safe for the route
-- registry broadcast path and prevents accidental serialisation of Lua
-- functions (which `json.encode` would reject).
function M.list()
    local out = {}
    for name, entry in pairs(registry.by_name) do
        out[#out + 1] = {
            name = name,
            path = entry.path,
            label = entry.label,
            icon = entry.icon,
            hide_from_nav = entry.hide_from_nav,
            order = entry.order,
            seq = entry.seq,
            source = entry.source,
        }
    end
    table.sort(out, function(a, b)
        local ao = a.order or math.huge
        local bo = b.order or math.huge
        if ao ~= bo then return ao < bo end
        if a.seq ~= b.seq then return a.seq < b.seq end
        return a.name < b.name
    end)
    return out
end

--- Build the `ui_route_registry_v1` payload from the registry.
--
-- Only surfaces with a `path` are included — that's the definition of a
-- routable page. `hide_from_nav` is passed through so the sidebar renderer
-- can filter at display time; we still ship it in the registry because
-- React Router needs to know about hidden paths in order to route to them.
-- @param hub_id string|nil
-- @return table
function M.build_route_registry_payload(hub_id)
    local routes = {}
    for _, entry in ipairs(M.list()) do
        if entry.path then
            routes[#routes + 1] = {
                path = entry.path,
                surface = entry.name,
                label = entry.label,
                icon = entry.icon,
                hide_from_nav = entry.hide_from_nav or nil,
            }
        end
    end
    return {
        type = "ui_route_registry_v1",
        hub_id = hub_id,
        routes = routes,
    }
end

--- Rust-facing entry point — called by `web_layout.render(name, state)` when
--- no override-chain layout table supplies `name`. Returns a UiNodeV1 table
--- or nil when the surface is not registered.
---
--- Errors raised by `render(state)` propagate up so the Rust fallback
--- wrapper sees them; `web_layout.render` already converts errors to the
--- error-fallback tree so a broken surface cannot crash the hub.
function M.render_node(name, render_state)
    local entry = M.get(name)
    if not entry then return nil end
    return entry.render(render_state)
end

--- Build per-subscription input state for `surface_name`. Uses the
--- surface's own `input_builder` when provided; otherwise delegates to the
--- default `LayoutInput.build_for_subscription`. Callers that want the
--- workspace-shaped `AgentWorkspaceSurfaceInputV1` get it for free; plugin
--- surfaces that need less can provide a tiny builder.
--- @param name string
--- @param client any Client instance (passed to input_builder)
--- @param sub_id string|nil
--- @return table input state (ready to hand to render())
function M.build_input(name, client, sub_id)
    local entry = M.get(name)
    if not entry then return nil end
    if entry.input_builder then
        return entry.input_builder(client, sub_id)
    end
    local LayoutInput = require("lib.layout_input")
    return LayoutInput.build_for_subscription(client, sub_id)
end

--- Test-only reset. Production never calls this — hot-reload preserves
--- state by design.
function M._reset_for_tests()
    for k in pairs(registry.by_name) do registry.by_name[k] = nil end
    registry.seq = 0
end

-- Hot-reload lifecycle
function M._before_reload()
    if log and log.info then log.info("surfaces.lua reloading") end
end

function M._after_reload()
    if log and log.info then log.info("surfaces.lua reloaded") end
end

return M
