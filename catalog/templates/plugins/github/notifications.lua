-- @template GitHub Integration
-- @description Subscribe to GitHub events and trigger agent workflows from issues/PRs
-- @category plugins
-- @dest plugins/github/notifications.lua
-- @scope device
-- @version 3.1.0

local M = {}

local Agent = require("lib.agent")
local hooks = require("hub.hooks")

function M.register()
    hooks.on("pty_notification", "github_question_notify", function(data)
        if data.already_notified then return end

        local session_uuid = data.session_uuid
        if not session_uuid then return end

        local agent = Agent.get(session_uuid)
        if not agent then return end

        if not agent:get_meta("issue_number") and not agent:get_meta("invocation_url") then return end

        local api_token = hub.api_token()
        if not api_token then return end

        local server_url = config.server_url()
        local server_id = hub.server_id()
        if not server_id then return end

        local notification_type = "question_asked"

        log.info(string.format("GitHub: posting %s notification for agent %s", notification_type, session_uuid))

        http.request({
            method = "POST",
            url = server_url .. "/api/hubs/" .. server_id .. "/notifications",
            headers = {
                ["Authorization"] = "Bearer " .. api_token,
                ["Content-Type"] = "application/json",
            },
            json = {
                repo = agent.repo,
                issue_number = agent:get_meta("issue_number"),
                invocation_url = agent:get_meta("invocation_url"),
                notification_type = notification_type,
            },
        }, function(resp, err)
            if err then
                log.warn(string.format("GitHub: notification post failed: %s", tostring(err)))
            elseif resp and resp.status >= 400 then
                log.warn(string.format("GitHub: notification post returned %d", resp.status))
            end
        end)
    end)
end

return M
