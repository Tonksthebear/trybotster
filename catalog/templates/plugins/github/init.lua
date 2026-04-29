-- @template GitHub Integration
-- @description Subscribe to GitHub events and trigger agent workflows from issues/PRs
-- @category plugins
-- @dest plugins/github/init.lua
-- @scope device
-- @version 3.1.0

-- GitHub Integration plugin entrypoint.
--
-- Product-specific GitHub behavior lives in this plugin directory. Core only
-- provides generic primitives: ActionCable, MCP proxying, hooks, HTTP, timers,
-- secrets, and agent/workspace helpers.

local mcp_proxy = require("mcp_proxy")
local notifications = require("notifications")
local event_routing = require("event_routing")

local repo = hub.detect_repo()
if not repo then
    log.info("GitHub plugin: disabled (no repo detected)")
    return {}
end

mcp_proxy.start()
notifications.register()
event_routing.start(repo)

log.info(string.format("GitHub plugin loaded for %s", repo))

return {
    _before_reload = function()
        event_routing.stop()
        mcp_proxy.stop()
    end,
}
