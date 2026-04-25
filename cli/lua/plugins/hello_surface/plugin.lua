-- Phase 4a/4b demo plugin — registers a hub-authored surface with multiple
-- sub-routes so the end-to-end substrate (surface registry +
-- ui_route_registry broadcast + sub-route dispatcher + DynamicSurface) can
-- be exercised without any Rails route changes or hand-crafted React
-- components.
--
-- Phase 4b switched the API: instead of a single `render` at a declared
-- `path`, a surface declares `routes = { { path, render }, ... }` and the
-- base URL is derived from the registration name. For "hello" the URL tree
-- is:
--
--   /hubs/<hub_id>/hello             → home (first route)
--   /hubs/<hub_id>/hello/details/:id → details page (pattern params)
--
-- This file deliberately shows a cross-sub-route link via `ctx.path(...)` so
-- the browser exercises the path-change → re-render wire design (new
-- `botster.surface.subpath` action).
--
-- ENV GATE (important): this file is only loaded by `hub/init.lua` when
-- `BOTSTER_DEV=1` OR `BOTSTER_ENV=test`. Production hubs skip it so real
-- users don't see the demo in their sidebar. If you're writing a real
-- plugin, do NOT copy this gating pattern — user plugins live under the
-- device root or a spawn target's repo and are picked up by the
-- ConfigResolver plugin loader. This file ships as part of the CLI so
-- dev/test hubs always exercise the substrate; that's the only reason it
-- sits under `cli/lua/plugins/`.
--
-- The spawn-target plugin loader does NOT scan `cli/lua/plugins/` — the
-- goal here is to ship the demo as part of the CLI itself. A real plugin
-- author follows the same `surfaces.register(...)` contract regardless of
-- where their `plugin.lua` lives.

local function home_page(_state, ctx)
    -- The link target is built via `ctx.path` so the URL always matches the
    -- surface's own base, even if somebody later renames the registration.
    -- Passing `{ id = 1 }` substitutes into ":id" in the pattern.
    local details_url = ctx.path("/details/:id", { id = 1 })
    return ui.stack{
        direction = "vertical",
        gap = "3",
        children = {
            ui.panel{
                tone = "default",
                border = true,
                children = {
                    ui.stack{
                        direction = "vertical",
                        gap = "2",
                        children = {
                            ui.text{
                                text = "Hello — Phase 4b sub-routes demo",
                                size = "md",
                                weight = "semibold",
                            },
                            ui.text{
                                text = "This surface declares TWO sub-routes: `/` (this page) and "
                                    .. "`/details/:id`. The hub's sub-route dispatcher extracts "
                                    .. ":id from the URL and threads it into the details render. "
                                    .. "Click below to navigate — the browser sends a "
                                    .. "`botster.surface.subpath` action and the hub re-renders "
                                    .. "just this surface with the new state.",
                                size = "sm",
                                tone = "muted",
                            },
                            ui.button{
                                label = "Open details for id=1",
                                icon = "arrow-right",
                                variant = "ghost",
                                tone = "default",
                                action = ui.action("botster.nav.open", { path = details_url }),
                            },
                        },
                    },
                },
            },
        },
    }
end

local function details_page(state, ctx)
    -- state.params.id comes from the `/details/:id` pattern match. Missing
    -- params should never happen here (the dispatcher only routes us here
    -- on a match) but defend anyway so a bad URL typed into the bar doesn't
    -- emit a nil-formatted string.
    local id = (state.params and state.params.id) or "?"
    local home_url = ctx.path("/")
    return ui.stack{
        direction = "vertical",
        gap = "3",
        children = {
            ui.panel{
                tone = "default",
                border = true,
                children = {
                    ui.stack{
                        direction = "vertical",
                        gap = "2",
                        children = {
                            ui.text{
                                text = string.format("Details for id=%s", id),
                                size = "md",
                                weight = "semibold",
                            },
                            ui.text{
                                text = "Subpath params are extracted by `lib.surfaces` and handed "
                                    .. "to your sub-route render as `state.params`. The surface "
                                    .. "base path and `ctx.path(...)` helper mean you never "
                                    .. "hardcode `/hubs/<hub_id>/...` — rename the registration "
                                    .. "and every link moves with it.",
                                size = "sm",
                                tone = "muted",
                            },
                            ui.button{
                                label = "Back to hello home",
                                icon = "arrow-left",
                                variant = "ghost",
                                tone = "default",
                                action = ui.action("botster.nav.open", { path = home_url }),
                            },
                        },
                    },
                },
            },
        },
    }
end

-- Surfaces with a simple `input_builder` decouple themselves from the
-- workspace payload — they don't need `AgentWorkspaceSurfaceInputV1`.
local function minimal_input(_client, _sub_id)
    local hub_id = (type(hub) == "table") and hub.server_id and hub.server_id() or nil
    return { hub_id = hub_id }
end

local entry = surfaces.register("hello", {
    label = "Hello",
    icon = "sparkle",
    order = 1000,
    source = "plugin:hello_surface",
    routes = {
        { path = "/",              render = home_page },
        { path = "/details/:id",   render = details_page },
    },
    input_builder = minimal_input,
})

log.info(string.format(
    "plugins/hello_surface: registered surface `%s` at %s (%d routes)",
    entry.name, tostring(entry.base_path), #entry.compiled_routes))

return true
