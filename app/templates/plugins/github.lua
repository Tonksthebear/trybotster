-- @template GitHub Integration
-- @description Subscribe to GitHub events and trigger agent workflows from issues/PRs
-- @category plugins
-- @dest shared/plugins/github/init.lua
-- @scope device
-- @version 3.0.0

-- GitHub event integration (plugin)
--
-- Subscribes to Github::EventsChannel for this repo and routes
-- incoming events to the command_message event system.
--
-- Agent matching: when a mention arrives, we check for an existing agent
-- that matches by issue number OR by branch name (for PRs routed to issues).
-- If found, the agent is notified instead of spawning a new one.
--
-- MCP token: on load, fetches a scoped MCP token from the Rails server
-- using the hub's auth, stores it in encrypted secrets, and registers
-- the remote MCP server as a proxy via mcp.proxy() — merging its tools
-- into botster-mcp so agents need only one MCP server entry.
--
-- Uses a separate ActionCable connection without crypto (GitHub
-- events are plaintext over TLS, no E2E encryption needed).

local Agent = require("lib.agent")
local hooks = require("hub.hooks")

local repo = hub.detect_repo()
if not repo then
    log.info("GitHub plugin: disabled (no repo detected)")
    return {}
end

-- ============================================================================
-- MCP Token Management
-- ============================================================================

--- Fetch a scoped MCP token from the Rails server and store it in secrets.
-- Uses the hub's API bearer token for auth. The MCP token is scoped to
-- agent-level operations only (GitHub tools, memory, etc.).
-- Synchronous — used once at plugin load before the event loop is busy.
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

--- Asynchronously fetch a fresh MCP token and store it in secrets.
-- Used for mid-session token refresh (e.g. after a 401 from the MCP server).
-- @param callback function(ok bool) Called when the fetch completes.
local function fetch_mcp_token_async(callback)
    local api_token = hub.api_token()
    if not api_token then
        log.warn("GitHub plugin: no API token available, cannot refresh MCP token")
        if callback then callback(false) end
        return
    end

    local server_url = config.server_url()
    http.request({
        method  = "POST",
        url     = server_url .. "/integrations/github/mcp_tokens",
        headers = {
            ["Authorization"] = "Bearer " .. api_token,
            ["Content-Type"]  = "application/json",
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

ensure_mcp_token()

-- ============================================================================
-- MCP Proxy Setup
-- ============================================================================

-- Forward-declared so on_mcp_auth_error (below) can reference it as an upvalue.
local setup_mcp_proxy

--- Handle a 401 from the remote MCP server: clear the stale token and re-fetch.
-- mcp.call_tool() invokes this when it receives a 401 response for a proxied tool.
local function on_mcp_auth_error()
    log.warn("GitHub plugin: MCP token rejected (401) — clearing token and re-fetching")
    -- Only clear the token; the MCP URL is stable and doesn't expire.
    -- Clearing mcp_url here would cause setup_mcp_proxy() to bail if the refresh
    -- response omits it (e.g. the endpoint only returns token on re-issue).
    secrets.set("github", "mcp_token", nil)
    fetch_mcp_token_async(function(ok)
        if ok then setup_mcp_proxy() end
    end)
end

--- Fetch the cached MCP URL + token and register the remote server as a proxy.
-- Safe to call repeatedly (refresh semantics — re-fetches the remote tool list).
setup_mcp_proxy = function()
    local mcp_url   = secrets.get("github", "mcp_url")
    local mcp_token = secrets.get("github", "mcp_token")
    if not mcp_url or not mcp_token then
        log.debug("GitHub plugin: no MCP URL/token cached, skipping proxy setup")
        return
    end
    mcp.proxy(mcp_url, { token = mcp_token, on_auth_error = on_mcp_auth_error })
end

setup_mcp_proxy()

-- Refresh the proxied tool list every 10 minutes so changes on the Rails side
-- propagate without a full plugin reload. Timer is guarded against double-registration
-- across hot-reloads via hub.state.
local _proxy_state = require("hub.state").get("github.mcp_proxy", {})
if not _proxy_state._started then
    _proxy_state._started = true
    _proxy_state.refresh_timer = timer.every(600, setup_mcp_proxy)
end

-- ============================================================================
-- Agent Matching
-- ============================================================================

--- Find an existing agent that matches a GitHub event by metadata.
-- Searches all running agents for matching repo + issue_number.
--
-- @param event_repo string "owner/repo"
-- @param payload table The event payload
-- @return Agent|nil
local function find_matching_agent(event_repo, payload)
    local issue_number = payload.issue_number
    if not issue_number then return nil end

    for _, agent in ipairs(Agent.find_by_meta("issue_number", issue_number)) do
        if agent.repo == event_repo then
            return agent
        end
    end

    -- PR routing: if this came from a PR routed to an issue, check the target
    local ctx = payload.structured_context
    if ctx and ctx.routed_to and ctx.routed_to.number then
        local target = ctx.routed_to.number
        for _, agent in ipairs(Agent.find_by_meta("issue_number", target)) do
            if agent.repo == event_repo then
                return agent
            end
        end
    end

    return nil
end

--- Format a notification for an existing agent about a new mention.
-- @param payload table The event payload
-- @return string
local function format_notification(payload)
    local prompt = payload.prompt or payload.context or payload.comment_body
    if prompt then
        return string.format(
            "=== NEW MENTION (automated notification) ===\n\n%s\n\n==================",
            prompt
        )
    end
    return "=== NEW MENTION (automated notification) ===\nNew mention\n=================="
end

--- Notify an existing agent by writing to its "agent" PTY session.
-- @param agent Agent
-- @param payload table
local function notify_agent(agent, payload)
    local session = agent.sessions and agent.sessions["agent"]
    if session then
        session:send_message(format_notification(payload))
        log.info(string.format("GitHub: notified existing agent %s", agent:agent_key()))
    else
        log.warn(string.format("GitHub: cannot notify agent %s (no 'agent' session)", agent:agent_key()))
    end
end

-- ============================================================================
-- PTY Notification Hook (agent asked a question)
-- ============================================================================

--- Post agent notification to Rails for GitHub integration.
-- When an agent sends an OSC notification (e.g., "question asked"),
-- this posts to Rails which can update the GitHub issue/PR.
hooks.on("pty_notification", "github_question_notify", function(data)
    -- Skip if agent already has a pending notification (avoid duplicate comments)
    if data.already_notified then return end

    local agent_key = data.agent_key
    if not agent_key then return end

    local agent = Agent.get(agent_key)
    if not agent then return end

    -- Only post if we have issue context
    if not agent:get_meta("issue_number") and not agent:get_meta("invocation_url") then return end

    local api_token = hub.api_token()
    if not api_token then return end

    local server_url = config.server_url()
    local server_id = hub.server_id()
    if not server_id then return end

    local notification_type = "question_asked"

    log.info(string.format("GitHub: posting %s notification for agent %s", notification_type, agent_key))

    http.request(server_url .. "/api/hubs/" .. server_id .. "/notifications", {
        method = "POST",
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
    })
end)

-- ============================================================================
-- Event Channel
-- ============================================================================

local conn = action_cable.connect()

-- The callback receives (message, channel_id) from the primitive,
-- so we use channel_id directly — no upvalue capture needed.
action_cable.subscribe(conn, "Github::EventsChannel",
    { repo = repo },
    function(message, channel_id)
        local payload = message.payload or {}
        local event_repo = message.repo or repo

        if message.event_type == "agent_cleanup" then
            -- PR closed or issue closed — delete matching agents by metadata
            if payload.issue_number then
                local matches = Agent.find_by_meta("issue_number", payload.issue_number)
                for _, agent in ipairs(matches) do
                    if agent.repo == event_repo then
                        events.emit("command_message", {
                            type = "delete_agent",
                            agent_id = agent:agent_key(),
                            delete_worktree = false,
                        })
                    end
                end
            end
        else
            -- New mention — find existing agent or create a new one
            local existing = find_matching_agent(event_repo, payload)
            if existing then
                notify_agent(existing, payload)
            else
                events.emit("command_message", {
                    type = "create_agent",
                    issue_or_branch = payload.issue_number and tostring(payload.issue_number),
                    prompt = payload.prompt or payload.context or payload.comment_body,
                    repo = event_repo,
                    metadata = {
                        issue_number = payload.issue_number,
                        invocation_url = payload.issue_url,
                    },
                })
            end
        end

        action_cable.perform(channel_id, "ack", { id = message.id })
    end
)

log.info(string.format("GitHub plugin loaded for %s", repo))

-- loader.lua calls _before_reload on the plugin's return value (package.loaded["plugin.github"]).
-- That's this table — not hub.state tables, which are never called by the loader.
return {
    _before_reload = function()
        if _proxy_state._started then
            timer.cancel(_proxy_state.refresh_timer)
            _proxy_state._started = false
        end
    end,
}
