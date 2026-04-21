-- Phase 4a demo plugin — registers a hub-authored surface at
-- `/plugins/hello` so the end-to-end substrate (surface registry +
-- ui_route_registry_v1 broadcast + DynamicSurfaceRoute) can be exercised
-- without any Rails route changes or hand-crafted React components.
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

local function build_tree(_state)
    return ui.stack{
        direction = "vertical",
        gap = "3",
        padding = "4",
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
                                text = "Hello from a plugin-registered surface",
                                size = "md",
                                weight = "semibold",
                            },
                            ui.text{
                                text = "This page is rendered by cli/lua/plugins/hello_surface/plugin.lua. "
                                    .. "It was reachable at /hubs/:hub_id/plugins/hello without a "
                                    .. "single Rails route, React component, or controller change — "
                                    .. "the hub broadcasts ui_route_registry_v1 and ui_layout_tree_v1, "
                                    .. "and the browser mounts a UiTree bound to this surface.",
                                size = "sm",
                                tone = "muted",
                            },
                            ui.text{
                                text = "Phase 4a: substrate proven.",
                                size = "xs",
                                tone = "accent",
                                monospace = true,
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
    path = "/plugins/hello",
    label = "Hello",
    icon = "sparkle",
    order = 1000,
    source = "plugin:hello_surface",
    render = build_tree,
    input_builder = minimal_input,
})

log.info(string.format(
    "plugins/hello_surface: registered surface `%s` at %s",
    entry.name, entry.path))

return true
