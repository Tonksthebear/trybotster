-- @template Demo Surface
-- @description Multi-file plugin showing web and TUI customization from init.lua
-- @category plugins
-- @dest plugins/demo-surface/tui/status.lua
-- @scope device
-- @version 1.0.0

botster.ui.register_component("demo_surface.status", function(_state)
    return {
        text = "Demo Surface",
        style = { fg = "cyan", bold = true },
    }
end)
