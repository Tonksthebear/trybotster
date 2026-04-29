-- @template GitHub Integration
-- @description Subscribe to GitHub events and trigger agent workflows from issues/PRs
-- @category plugins
-- @dest plugins/github/mcp_proxy.lua
-- @scope device
-- @version 3.1.0

local M = {}

local state = require("hub.state")
local proxy_state = state.get("github.mcp_proxy", {})

local setup_mcp_proxy

local function ensure_mcp_token()
    local cached = secrets.get("github", "mcp_token")
    if cached then
        log.debug("GitHub plugin: using cached MCP token")
        return
    end

    local api_token = hub.api_token()
    if not api_token then
        log.warn("GitHub plugin: no API token available, skipping MCP token fetch")
        return
    end

    local server_url = config.server_url()
    local resp, err = http.post(server_url .. "/integrations/github/mcp_tokens", {
        headers = { ["Authorization"] = "Bearer " .. api_token },
        json = {},
    })

    if err then
        log.warn(string.format("GitHub plugin: failed to fetch MCP token: %s", tostring(err)))
        return
    end

    if resp.status ~= 200 and resp.status ~= 201 then
        log.warn(string.format("GitHub plugin: MCP token request returned %d", resp.status))
        return
    end

    local body = json.decode(resp.body)
    if body and body.token then
        secrets.set("github", "mcp_token", body.token)
        if body.mcp_url then
            secrets.set("github", "mcp_url", body.mcp_url)
        end
        log.info("GitHub plugin: MCP token fetched and stored")
    end
end

local function fetch_mcp_token_async(callback)
    local api_token = hub.api_token()
    if not api_token then
        log.warn("GitHub plugin: no API token available, cannot refresh MCP token")
        if callback then callback(false) end
        return
    end

    local server_url = config.server_url()
    http.request({
        method = "POST",
        url = server_url .. "/integrations/github/mcp_tokens",
        headers = {
            ["Authorization"] = "Bearer " .. api_token,
            ["Content-Type"] = "application/json",
        },
        body = "{}",
    }, function(resp, err)
        if err then
            log.warn(string.format("GitHub plugin: MCP token refresh failed: %s", tostring(err)))
            if callback then callback(false) end
            return
        end
        if resp.status ~= 200 and resp.status ~= 201 then
            log.warn(string.format("GitHub plugin: MCP token refresh returned %d", resp.status))
            if callback then callback(false) end
            return
        end
        local body = json.decode(resp.body)
        if body and body.token then
            secrets.set("github", "mcp_token", body.token)
            if body.mcp_url then
                secrets.set("github", "mcp_url", body.mcp_url)
            end
            log.info("GitHub plugin: MCP token refreshed and stored")
            if callback then callback(true) end
        else
            log.warn("GitHub plugin: MCP token refresh response missing token field")
            if callback then callback(false) end
        end
    end)
end

local function on_mcp_auth_error()
    log.warn("GitHub plugin: MCP token rejected (401), clearing token and re-fetching")
    secrets.set("github", "mcp_token", nil)
    fetch_mcp_token_async(function(ok)
        if ok then setup_mcp_proxy() end
    end)
end

setup_mcp_proxy = function()
    local mcp_url = secrets.get("github", "mcp_url")
    local mcp_token = secrets.get("github", "mcp_token")
    if not mcp_url or not mcp_token then
        log.debug("GitHub plugin: no MCP URL/token cached, skipping proxy setup")
        return
    end
    mcp.proxy(mcp_url, { token = mcp_token, on_auth_error = on_mcp_auth_error })
end

function M.start()
    ensure_mcp_token()
    setup_mcp_proxy()

    if not proxy_state.started then
        proxy_state.started = true
        proxy_state.refresh_timer = timer.every(600, setup_mcp_proxy)
    end
end

function M.stop()
    if proxy_state.started then
        timer.cancel(proxy_state.refresh_timer)
        proxy_state.refresh_timer = nil
        proxy_state.started = false
    end
end

return M
