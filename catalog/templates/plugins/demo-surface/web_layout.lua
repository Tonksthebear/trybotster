-- @template Demo Surface
-- @description Multi-file plugin showing web and TUI customization from init.lua
-- @category plugins
-- @dest plugins/demo-surface/web_layout.lua
-- @scope device
-- @version 1.0.0

local M = {}

function M.home()
    return {
        type = "panel",
        props = { title = "Demo Surface" },
        children = {
            {
                type = "text",
                props = {
                    text = "This web surface is rendered from web_layout.lua, required by init.lua.",
                },
            },
        },
    }
end

return M
