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
-- using the hub's auth, stores it in encrypted secrets, and injects it
-- into agent environments via the filter_agent_env hook.
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
local function ensure_mcp_token()
    -- Check if we already have a cached token
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

ensure_mcp_token()

-- Inject MCP token into agent environments so agents can use the MCP server.
hooks.intercept("filter_agent_env", "github_mcp_token", function(env)
    local token = secrets.get("github", "mcp_token")
    if token then
        env.BOTSTER_MCP_TOKEN = token
    end
    local mcp_url = secrets.get("github", "mcp_url")
    if mcp_url then
        env.BOTSTER_MCP_URL = mcp_url
    end
    return env
end)

-- ============================================================================
-- Agent Matching
-- ============================================================================

--- Find an existing agent that matches a GitHub event.
-- Checks by issue number first, then by branch name pattern.
-- PR events routed to linked issues will match the issue agent.
--
-- @param event_repo string "owner/repo"
-- @param payload table The event payload
-- @return Agent|nil
local function find_matching_agent(event_repo, payload)
    local issue_number = payload.issue_number
    if not issue_number then return nil end

    local repo_safe = event_repo:gsub("/", "-")

    -- Direct match: agent working on this issue number
    local by_issue = Agent.get(repo_safe .. "-" .. tostring(issue_number))
    if by_issue then return by_issue end

    -- Branch pattern match: agent on botster-issue-N branch
    local by_branch = Agent.get(repo_safe .. "-botster-issue-" .. tostring(issue_number))
    if by_branch then return by_branch end

    -- PR routing: if this came from a PR routed to an issue, the structured_context
    -- tells us the target issue number. Check for an agent on that issue.
    local ctx = payload.structured_context
    if ctx and ctx.routed_to and ctx.routed_to.number then
        local target = ctx.routed_to.number
        local by_routed = Agent.get(repo_safe .. "-" .. tostring(target))
        if by_routed then return by_routed end

        by_routed = Agent.get(repo_safe .. "-botster-issue-" .. tostring(target))
        if by_routed then return by_routed end
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
        session:write(format_notification(payload) .. "\r\r")
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
    local agent_key = data.agent_key
    if not agent_key then return end

    local agent = Agent.get(agent_key)
    if not agent then return end

    -- Only post if we have issue context
    if not agent.issue_number and not agent.invocation_url then return end

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
            issue_number = agent.issue_number,
            invocation_url = agent.invocation_url,
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
            -- PR closed or issue closed — delete the matching agent
            local repo_safe = event_repo:gsub("/", "-")
            if payload.issue_number then
                events.emit("command_message", {
                    type = "delete_agent",
                    agent_id = repo_safe .. "-" .. tostring(payload.issue_number),
                    delete_worktree = false,
                })
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
                    invocation_url = payload.issue_url,
                })
            end
        end

        action_cable.perform(channel_id, "ack", { id = message.id })
    end
)

log.info(string.format("GitHub plugin loaded for %s", repo))
return {}
