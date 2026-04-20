-- Bootstrap registrations for the built-in workspace surfaces.
--
-- Prior to Phase 4a, `workspace_sidebar` and `workspace_panel` were hardcoded
-- in `lib.layout_broadcast` (the two-density SURFACE_TARGETS table) and
-- rendered directly by `web_layout.render("workspace_surface", state)`. The
-- surface registry (`lib.surfaces`) is now the single source of truth for
-- browser surfaces, so both densities are registered here as first-class
-- surfaces whose render function delegates to the existing embedded
-- `workspace_surface` layout. This preserves the override-file chain — a user
-- layout_web.lua that defines `workspace_surface` still wins — and keeps the
-- Lua-authored tree identical to Phase 2 output.

local surfaces = require("lib.surfaces")

-- Shallow-copy `base` and set `surface` to the density the embedded
-- `workspace_surface` layout expects. `web.layout:workspace_surface` branches
-- on this field to pick sidebar-vs-panel sizing.
local function with_density(base, density)
    local out = {}
    if type(base) == "table" then
        for k, v in pairs(base) do out[k] = v end
    end
    out.surface = density
    return out
end

-- Both surfaces share the same embedded layout function; the only difference
-- is the `surface` density hint injected into the state. The wrapper calls
-- the Rust `web_layout.render` primitive (not `surfaces.render_node`) so
-- override files for `workspace_surface` take effect for free.
local function workspace_renderer(density)
    return function(render_state)
        local state_with_density = with_density(render_state, density)
        local tree_json = web_layout.render("workspace_surface", state_with_density)
        if type(tree_json) ~= "string" then
            error(string.format(
                "workspace_renderer(%s): web_layout.render returned %s, expected string",
                density, type(tree_json)))
        end
        local tree, decode_err = json.decode(tree_json)
        if type(tree) ~= "table" then
            error(string.format(
                "workspace_renderer(%s): failed to decode layout JSON: %s",
                density, tostring(decode_err)))
        end
        return tree
    end
end

-- Ordering rationale:
--   * workspace_panel is the hub landing page ("/"); `order = 0` pins it to
--     the top of the nav, ahead of any plugin-registered surfaces (which
--     default to `order = nil` == math.huge).
--   * workspace_sidebar is the sidebar-only surface — no path, no nav entry;
--     `order` is irrelevant but keeping it with the panel at 0 keeps debug
--     output grouped.
surfaces.register("workspace_sidebar", {
    render = workspace_renderer("sidebar"),
    order = 0,
    source = "builtin",
    -- no path: sidebar is not a routable page
})

surfaces.register("workspace_panel", {
    path = "/",
    label = "Hub",
    icon = "home",
    render = workspace_renderer("panel"),
    order = 0,
    source = "builtin",
})

return true
