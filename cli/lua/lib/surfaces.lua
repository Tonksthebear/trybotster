-- Surface registry (Phase 4a + 4b) — the Lua-side source of truth for browser
-- surfaces. A "surface" is the unit the hub composes and ships to browsers:
--   * it has a stable `name` (the `target_surface` on the wire)
--   * a `routes = { ... }` array OR a single `render(state)` function
--   * optional `input_builder(client, sub_id)` for per-subscription state
--
-- Phase 4b introduces sub-routes:
--
--   surfaces.register("kanban", {
--       label = "Kanban",
--       icon  = "squares-2x2",
--       -- base path = "/kanban" (derived from name). URL =
--       -- /hubs/<hub_id>/kanban. Each route's `path` is relative to that base.
--       routes = {
--           { path = "/",           render = kanban_home },
--           { path = "/board/:id",  render = kanban_board },
--           { path = "/settings",   render = kanban_settings },
--       },
--       input_builder = function(client, sub_id) ... end,
--   })
--
-- The sub-route's `render(state, ctx)` receives:
--   * `state.path`   — the subpath the browser is currently on ("/board/42")
--   * `state.params` — the named captures from the matched pattern
--                      (`{ id = "42" }` for "/board/:id")
--   * `ctx.hub_id`, `ctx.surface`, `ctx.base_path`
--   * `ctx.path(subpath, params)` — build a full `/hubs/<hub_id>/<base>/<sub>`
--     URL with `:name` interpolation from `params`.
--
-- Cross-surface helper:
--
--   surfaces.path("kanban", "/board/:id", { id = 42 })
--     -> "/hubs/<current-hub>/kanban/board/42"
--
-- Backwards compatibility: registrations that pass a single top-level `render`
-- (and optional top-level `path` for the URL base) are still accepted and
-- internally wrapped into `routes = { { path = "/", render = opts.render } }`.
-- The top-level `path` becomes the base for that surface (this is the escape
-- hatch built-in surfaces like `workspace_panel` use to keep their URL at "/").
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
--   3. `handlers.connections` broadcasts `ui_route_registry` on every
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
--       name, base_path, label, icon,
--       render,                -- auto-generated dispatcher (or passthrough)
--       compiled_routes,       -- array of { pattern, param_names, regex, render }
--       input_builder,
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

-- Escape Lua pattern metacharacters so literal path segments can be matched
-- via `string.match`. `/board/42.json` must match the literal pattern "/board/42.json"
-- exactly; the dot must not act as a Lua pattern wildcard. Belt-and-suspenders
-- — plugin-declared patterns are short strings with tight alphabets, but we
-- don't want to constrain that in the future.
local LUA_PATTERN_MAGIC = "([%(%)%.%%%+%-%*%?%[%]%^%$])"
local function escape_lua_pattern(s)
    return (s:gsub(LUA_PATTERN_MAGIC, "%%%1"))
end

-- Normalise a pattern fragment to start with "/". Plugins may write
-- `path = "board/:id"` (no leading slash) — accept it.
local function ensure_leading_slash(s)
    if s == nil or s == "" then return "/" end
    return s:sub(1, 1) == "/" and s or ("/" .. s)
end

-- Strip a trailing slash unless it's the root "/".
local function strip_trailing_slash(s)
    if s == "/" or s == "" then return "/" end
    return s:gsub("/+$", "")
end

-- Compile a route pattern into a matcher.
--
-- Pattern syntax:
--   * "/" — matches the empty/root subpath ("" or "/").
--   * Literal segments like "/settings" — exact match after normalisation.
--   * Named params like "/board/:id" — `:id` captures one segment (no slashes)
--     and is exposed as `params.id`.
--
-- Returns a table:
--   { pattern, lua_pattern, param_names, render }
local function compile_route(path_pattern, render)
    assert(type(render) == "function",
        "surfaces.register: each route must declare `render = function(state, ctx) ... end`")
    local normalised = ensure_leading_slash(path_pattern)
    normalised = strip_trailing_slash(normalised)
    if normalised == "" then normalised = "/" end

    local param_names = {}
    if normalised == "/" then
        return {
            pattern = "/",
            lua_pattern = "^/?$",
            param_names = param_names,
            render = render,
        }
    end

    local parts = { "^" }
    for segment in normalised:gmatch("[^/]+") do
        if segment:sub(1, 1) == ":" then
            local name = segment:sub(2)
            assert(name ~= "",
                "surfaces.register: empty param name in route pattern `" .. path_pattern .. "`")
            param_names[#param_names + 1] = name
            parts[#parts + 1] = "/([^/]+)"
        else
            parts[#parts + 1] = "/" .. escape_lua_pattern(segment)
        end
    end
    -- Accept either the exact form or a trailing slash so nav from `/board/42`
    -- vs `/board/42/` lands the same route.
    parts[#parts + 1] = "/?$"
    return {
        pattern = normalised,
        lua_pattern = table.concat(parts),
        param_names = param_names,
        render = render,
    }
end

-- Match a subpath against a single compiled route.
-- @return params table on match, nil otherwise.
local function match_compiled_route(compiled, subpath)
    local s = ensure_leading_slash(subpath or "/")
    -- Strip any query/fragment that leaked through — we only route by path.
    s = s:gsub("[?#].*$", "")
    -- For non-root routes, drop a trailing slash so "/board/42/" behaves like
    -- "/board/42". The compiled regex already tolerates it but the `/` form
    -- is canonical. Root stays root.
    if s ~= "/" then s = strip_trailing_slash(s) end

    if #compiled.param_names == 0 then
        if s:match(compiled.lua_pattern) then return {} end
        return nil
    end
    local captures = { s:match(compiled.lua_pattern) }
    if captures[1] == nil then return nil end
    local params = {}
    for i, name in ipairs(compiled.param_names) do
        params[name] = captures[i]
    end
    return params
end

-- Match `subpath` against each compiled route in order; return the first one
-- that matches along with its extracted params, or nil.
local function match_routes(compiled_routes, subpath)
    for _, compiled in ipairs(compiled_routes) do
        local params = match_compiled_route(compiled, subpath)
        if params ~= nil then
            return compiled, params
        end
    end
    return nil, nil
end

-- Build a full hub-scoped URL by combining the hub_id, surface's base path,
-- and a (possibly templated) subpath. `params` supplies values for `:name`
-- placeholders in the subpath.
--
-- Rules:
--   * `hub_id` must be a non-empty string; assertion otherwise.
--   * `base_path` must start with "/"; "/" means "the hub root".
--   * `subpath` may be "", "/", or start with "/". `:name` interpolated from
--     params; missing params are left as `:name` literals so test output is
--     easy to spot (and we don't paper over a bug).
local function build_url(hub_id, base_path, subpath, params)
    assert(is_nonempty_string(hub_id),
        "surfaces.path: hub_id is required (pass it explicitly or ensure hub.server_id() is set)")
    local base = base_path
    if base == nil or base == "" then base = "/" end
    base = ensure_leading_slash(base)

    local sub = subpath
    if sub == nil or sub == "" then sub = "/" end
    sub = ensure_leading_slash(sub)

    -- Interpolate params into the subpath. Missing params leave the literal
    -- `:name` token so tests (and plugin authors) can spot the miss.
    if params and next(params) ~= nil then
        sub = sub:gsub(":([%w_]+)", function(name)
            local v = params[name]
            if v == nil then return ":" .. name end
            return tostring(v)
        end)
    end

    -- Assemble: /hubs/<hub_id> + base + sub. Avoid double slashes.
    local prefix = "/hubs/" .. hub_id
    local path
    if base == "/" then
        path = prefix
    else
        path = prefix .. strip_trailing_slash(base)
    end
    if sub == "/" then
        if base == "/" then
            -- Root hub page: prefer "/hubs/<id>" (no trailing slash)
            return path
        end
        return path
    end
    return path .. sub
end

-- Construct the `ctx` table threaded to a sub-route's render fn.
local function build_ctx(hub_id, surface_name, base_path)
    return {
        hub_id = hub_id,
        surface = surface_name,
        base_path = base_path,
        path = function(sub, params)
            return build_url(hub_id, base_path, sub, params)
        end,
    }
end

-- Derive the base path for a surface from its `name` unless `opts.path` is
-- explicitly set (back-compat escape hatch used by `workspace_panel` to keep
-- its URL at "/"). Pure plugin-authored surfaces always use the name-derived
-- default so `surfaces.register("kanban", { routes = {...} })` is enough.
local function derive_base_path(name, opts)
    if is_nonempty_string(opts.path) then
        return ensure_leading_slash(opts.path)
    end
    return "/" .. name
end

-- Build the array of compiled routes from the user's registration, applying
-- the back-compat wrapper (top-level `render` → `routes = {{"/", render}}`).
local function compile_routes(name, opts)
    if opts.routes ~= nil then
        assert(type(opts.routes) == "table" and #opts.routes > 0,
            "surfaces.register(" .. name .. "): `routes` must be a non-empty array")
        assert(opts.render == nil,
            "surfaces.register(" .. name .. "): pass EITHER `routes` OR a single top-level `render`, not both")
        local compiled = {}
        for i, route in ipairs(opts.routes) do
            assert(type(route) == "table",
                "surfaces.register(" .. name .. "): routes[" .. i .. "] must be a table")
            compiled[#compiled + 1] = compile_route(route.path or "/", route.render)
        end
        return compiled
    end
    assert(type(opts.render) == "function",
        "surfaces.register(" .. name .. "): must provide `routes` or a top-level `render`")
    return { compile_route("/", opts.render) }
end

-- Build the sub-route 404 fallback tree. Called when a surface's subpath
-- doesn't match any declared route. Uses only v1 primitives so the browser
-- renders it without Phase-4b client support.
local function render_sub_404(surface_name, subpath)
    return {
        type = "panel",
        props = { tone = "muted", border = true },
        children = {
            {
                type = "stack",
                props = { direction = "vertical", gap = "2" },
                children = {
                    {
                        type = "text",
                        props = {
                            text = "Sub-route not found",
                            size = "md",
                            weight = "semibold",
                            tone = "danger",
                        },
                    },
                    {
                        type = "text",
                        props = {
                            text = string.format(
                                "Surface `%s` has no route matching `%s`.",
                                surface_name, subpath or "/"
                            ),
                            size = "sm",
                            tone = "muted",
                        },
                    },
                },
            },
        },
    }
end

-- Generate the top-level `entry.render(state)` dispatcher. Always goes through
-- this even in the single-route back-compat case so `ctx` is available to the
-- user's render fn uniformly.
local function make_dispatcher(name, base_path, compiled_routes)
    return function(render_state)
        local s = type(render_state) == "table" and render_state or {}
        local subpath = ensure_leading_slash(s.path or "/")
        local compiled, params = match_routes(compiled_routes, subpath)
        local hub_id = s.hub_id
        local ctx = build_ctx(hub_id, name, base_path)
        if compiled == nil then
            log.warn(string.format(
                "surfaces: no route matches subpath `%s` for surface `%s`",
                subpath, name))
            return render_sub_404(name, subpath)
        end
        -- Build a shallow-copied sub-state so the dispatcher's writes
        -- (path/params) don't mutate the caller's state — some callers
        -- reuse `state` across multiple surface renders.
        local sub_state = {}
        for k, v in pairs(s) do sub_state[k] = v end
        sub_state.path = subpath
        sub_state.params = params
        return compiled.render(sub_state, ctx)
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

    local existing = registry.by_name[name]
    local seq = existing and existing.seq or (registry.seq + 1)
    if not existing then registry.seq = seq end

    local compiled_routes = compile_routes(name, opts)
    local base_path = derive_base_path(name, opts)
    local render = make_dispatcher(name, base_path, compiled_routes)

    local entry = {
        name = name,
        base_path = base_path,
        -- `path` is preserved for consumers that still read the top-level
        -- field (sidebar nav, route registry payload). It equals base_path
        -- for routable surfaces; `nil` for surfaces that opted out via
        -- `hide_from_nav` OR passed an empty routes+render pair. Since the
        -- Phase-4b default always derives a base_path, `path` defaults to
        -- it — meaning every registered surface is routable unless the
        -- plugin explicitly sets `path = false` (or similar) to suppress it.
        path = base_path,
        label = is_nonempty_string(opts.label) and opts.label or name,
        icon = is_nonempty_string(opts.icon) and opts.icon or nil,
        render = render,
        compiled_routes = compiled_routes,
        input_builder = type(opts.input_builder) == "function" and opts.input_builder or nil,
        hide_from_nav = opts.hide_from_nav == true,
        order = type(opts.order) == "number" and opts.order or nil,
        source = is_nonempty_string(opts.source) and opts.source or nil,
        seq = seq,
    }
    -- A small number of legacy surfaces (workspace_sidebar) register WITHOUT
    -- wanting to appear as a routable page. They omit both `path` and
    -- `routes` in the old API; in the new API they still omit `routes` and
    -- we must keep their `path` nil. Detect the "no-path, no-routes, just a
    -- render" shape by checking that the caller did not pass `routes` AND
    -- did not pass `path`: that means they registered for multi-surface
    -- broadcast only, not for a URL route. In that case, strip `path`.
    if opts.routes == nil and not is_nonempty_string(opts.path) then
        entry.path = nil
        entry.base_path = nil
    end
    registry.by_name[name] = entry

    if log and log.debug then
        log.debug(string.format(
            "surfaces.register: name=%s base_path=%s label=%s routes=%d",
            name, tostring(entry.base_path), tostring(entry.label), #compiled_routes))
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
    -- Wire protocol v2: purge tree_snapshot's dedup baselines for this
    -- surface across all subpaths. Without this a re-registration of the
    -- same surface name could be silently swallowed by dedup if the new
    -- tree happened to hash-match the stale one. pcall in case
    -- lib.tree_snapshot isn't loaded (test harnesses that only import
    -- surfaces.lua).
    local ok_snap, snap = pcall(require, "lib.tree_snapshot")
    if ok_snap and type(snap) == "table"
        and type(snap.forget_surface) == "function"
    then
        pcall(snap.forget_surface, name)
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

--- Build the canonical URL for a surface's subpath.
---
--- Cross-surface link helper. Pulls the hub_id from `hub.server_id()` so the
--- caller doesn't have to thread it through.
---
--- Examples:
---   surfaces.path("kanban", "/board/:id", { id = 42 })
---     -> "/hubs/<hub_id>/kanban/board/42"
---   surfaces.path("kanban")
---     -> "/hubs/<hub_id>/kanban"
---
--- @param name string Registered surface name.
--- @param subpath string? Subpath within the surface; defaults to "/".
--- @param params table? `:name` param substitutions.
--- @return string|nil Full URL, or nil when the surface is unknown or has no base_path.
function M.path(name, subpath, params)
    local entry = M.get(name)
    if not entry then return nil end
    if not is_nonempty_string(entry.base_path) then
        return nil
    end
    local hub_id
    if type(hub) == "table" and type(hub.server_id) == "function" then
        local ok, id = pcall(hub.server_id)
        if ok and is_nonempty_string(id) then hub_id = id end
    end
    if not is_nonempty_string(hub_id) then
        return nil
    end
    return build_url(hub_id, entry.base_path, subpath or "/", params)
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
            base_path = entry.base_path,
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

--- Build the `ui_route_registry` payload from the registry.
--
-- Only surfaces with a `path` are included — that's the definition of a
-- routable page. `hide_from_nav` is passed through so the sidebar renderer
-- can filter at display time; we still ship it in the registry because
-- React Router needs to know about hidden paths in order to route to them.
--
-- Phase 4b: each registry entry also carries `routes` (the declared
-- sub-patterns) so the browser can optionally pattern-match before firing
-- its first subpath action. The hub remains the authority — mismatches
-- fall through to the hub's auto-dispatcher 404 — but exposing the list
-- lets the browser render an offline-friendly loading state.
-- @param hub_id string|nil
-- @return table
function M.build_route_registry_payload(hub_id)
    local routes = {}
    for _, summary in ipairs(M.list()) do
        if summary.path then
            local entry = registry.by_name[summary.name]
            local sub_patterns = {}
            if entry and entry.compiled_routes then
                for _, compiled in ipairs(entry.compiled_routes) do
                    sub_patterns[#sub_patterns + 1] = { path = compiled.pattern }
                end
            end
            routes[#routes + 1] = {
                path = summary.path,
                base_path = summary.base_path,
                surface = summary.name,
                label = summary.label,
                icon = summary.icon,
                hide_from_nav = summary.hide_from_nav or nil,
                routes = sub_patterns,
            }
        end
    end
    return {
        type = "ui_route_registry",
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
---
--- Phase 4b ergonomics: when `route_ctx` is passed, it threads to the
--- plugin-authored `input_builder` as a third argument so per-sub-route
--- data-loading (e.g. `/board/:id` loading board N) can happen at
--- input-build time instead of inside render. Backwards compatible —
--- existing 2-arg input_builders simply ignore the extra arg.
--- @param name string
--- @param client any Client instance (passed to input_builder)
--- @param sub_id string|nil
--- @param route_ctx table|nil { path = string, params = table }
--- @return table input state (ready to hand to render())
function M.build_input(name, client, sub_id, route_ctx)
    local entry = M.get(name)
    if not entry then return nil end
    if entry.input_builder then
        return entry.input_builder(client, sub_id, route_ctx)
    end
    -- Wire protocol v2 default: trees no longer carry per-client selection
    -- or pre-fetched session lists. The composite primitives (session_list,
    -- workspace_list, …) read from the client-side entity stores. Built-in
    -- surfaces without an input_builder receive only the hub identity.
    return {
        hub_id = hub.server_id and hub.server_id() or nil,
    }
end

--- Resolve the compiled route + params for `subpath` against surface `name`.
---
--- Pure lookup — does not render. Lets callers (notably
--- `lib.layout_broadcast`) compute `{ path, params }` before calling the
--- surface's `input_builder` so per-sub-route data loading has access to
--- the matched params.
---
--- @param name string Registered surface name.
--- @param subpath string? Subpath to match (defaults to "/").
--- @return table|nil compiled_route The matched compiled route (internal shape), or nil when no route matches.
--- @return table|nil params Named captures from the match, or nil.
function M.resolve_route(name, subpath)
    local entry = M.get(name)
    if not entry or type(entry.compiled_routes) ~= "table" then
        return nil, nil
    end
    return match_routes(entry.compiled_routes, subpath or "/")
end

--- Test-only reset. Production never calls this — hot-reload preserves
--- state by design.
function M._reset_for_tests()
    for k in pairs(registry.by_name) do registry.by_name[k] = nil end
    registry.seq = 0
end

-- Test-only export: expose internal helpers for unit-level coverage without
-- requiring the full layout broadcast wire-up.
M._build_url = build_url
M._compile_route = compile_route
M._match_routes = match_routes

-- Hot-reload lifecycle
function M._before_reload()
    if log and log.info then log.info("surfaces.lua reloading") end
end

function M._after_reload()
    if log and log.info then log.info("surfaces.lua reloaded") end
end

return M
