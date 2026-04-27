-- @template Demo Surface
-- @description Multi-file plugin showing web and TUI customization from init.lua
-- @category plugins
-- @dest plugins/demo-surface/init.lua
-- @scope device
-- @version 1.0.0
-- @tui tui/status.lua

local surfaces = require("lib.surfaces")
local web_layout = require("web_layout")

surfaces.register("demo_surface", {
    label = "Demo Surface",
    icon = "sparkles",
    clients = { "web" },
    routes = {
        { path = "/", render = web_layout.home },
    },
})

log.info("Demo Surface plugin loaded")

return {}
