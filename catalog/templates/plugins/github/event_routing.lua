-- @template GitHub Integration
-- @description Subscribe to GitHub events and trigger agent workflows from issues/PRs
-- @category plugins
-- @dest plugins/github/event_routing.lua
-- @scope device
-- @version 3.1.0

local M = {}

local Agent = require("lib.agent")
local state = require("hub.state")
local routing_state = state.get("github.event_routing", {})

local function github_workspace_name(repo, issue_number, branch_name)
    if issue_number then
        return repo .. "#" .. tostring(issue_number)
    end
    return repo .. ":" .. (branch_name or "main")
end

local function find_matching_agent(event_repo, payload)
    local issue_number = payload.issue_number
    if not issue_number then return nil end

    local ws_name = github_workspace_name(event_repo, issue_number)
    local matches = Agent.find_by_workspace(ws_name)
    if #matches > 0 then return matches[1] end

    local ctx = payload.structured_context
    if ctx and ctx.routed_to and ctx.routed_to.number then
        local target_name = github_workspace_name(event_repo, ctx.routed_to.number)
        local target_matches = Agent.find_by_workspace(target_name)
        if #target_matches > 0 then return target_matches[1] end
    end

    return nil
end

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

local function notify_agent(agent, payload)
    if agent.session then
        agent.session:send_message(format_notification(payload))
        log.info(string.format("GitHub: notified existing agent %s", agent.session_uuid))
    else
        log.warn(string.format("GitHub: cannot notify agent %s (no session)", agent.session_uuid))
    end
end

local function handle_message(default_repo, message, channel_id)
    local payload = message.payload or {}
    local event_repo = message.repo or default_repo

    if message.event_type == "agent_cleanup" then
        if payload.issue_number then
            local ws_name = github_workspace_name(event_repo, payload.issue_number)
            local matches = Agent.find_by_workspace(ws_name)
            for _, agent in ipairs(matches) do
                events.emit("command_message", {
                    type = "delete_agent",
                    agent_id = agent.session_uuid,
                    delete_worktree = false,
                })
            end
        end
    else
        local existing = find_matching_agent(event_repo, payload)
        if existing then
            notify_agent(existing, payload)
        else
            local issue_num = payload.issue_number
            local ws_name = github_workspace_name(event_repo, issue_num)
            events.emit("command_message", {
                type = "create_agent",
                issue_or_branch = issue_num and tostring(issue_num),
                prompt = payload.prompt or payload.context or payload.comment_body,
                repo = event_repo,
                metadata = {
                    issue_number = issue_num,
                    invocation_url = payload.issue_url,
                    workspace = ws_name,
                    workspace_metadata = { repo = event_repo, issue_number = issue_num },
                },
            })
        end
    end

    action_cable.perform(channel_id, "ack", { id = message.id })
end

function M.start(repo)
    M.stop()

    routing_state.conn = action_cable.connect()
    routing_state.channel = action_cable.subscribe(
        routing_state.conn,
        "Github::EventsChannel",
        { repo = repo },
        function(message, channel_id)
            handle_message(repo, message, channel_id)
        end
    )
end

function M.stop()
    if routing_state.conn then
        action_cable.close(routing_state.conn)
        routing_state.conn = nil
        routing_state.channel = nil
    end
end

return M
