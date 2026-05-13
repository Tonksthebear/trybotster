-- @template GitHub Integration
-- @description Subscribe to GitHub events and trigger agent workflows from issues/PRs
-- @category plugins
-- @dest plugins/github/event_routing.lua
-- @scope device
-- @version 3.1.0

local M = {}

local Agent = require("lib.agent")
local Hub = require("lib.hub")
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
    local ok, err = pcall(function()
        Hub.get():send_message(agent.session_uuid, format_notification(payload))
    end)
    if ok then
        log.info(string.format("GitHub: notified existing agent %s", agent.session_uuid))
    else
        log.warn(string.format("GitHub: cannot notify agent %s: %s", agent.session_uuid, tostring(err)))
    end
end

local function pr_payload(message, payload)
    local pr = payload.pull_request or payload.pr or {}
    local repo = message.repo
        or payload.repo
        or payload.repository_full_name
        or (payload.repository and payload.repository.full_name)
    local number = payload.pr_number
        or payload.pull_request_number
        or payload.number
        or pr.number
    local action = message.action or payload.action or message.event_type or payload.event_type
    local merged = payload.merged == true or pr.merged == true or action == "merged" or action == "closed_merged"

    if not number or not merged then
        return nil
    end

    return {
        provider = "github",
        repo = repo,
        pr_number = tonumber(number),
        pr_url = payload.pr_url or payload.html_url or pr.html_url or pr.url,
        head_branch = payload.head_branch or (pr.head and pr.head.ref),
        base_branch = payload.base_branch or (pr.base and pr.base.ref),
        merge_commit = payload.merge_commit or payload.merge_commit_sha or pr.merge_commit_sha,
        merged_at = payload.merged_at or pr.merged_at,
        raw_event_type = message.event_type or payload.event_type,
    }
end

local function emit_pr_merged(event_repo, message, payload)
    local event = pr_payload(message, payload)
    if not event then
        return false, false
    end
    event.repo = event.repo or event_repo
    if not event.repo then
        return true, false
    end
    if events and events.emit then
        local ok, err = pcall(events.emit, "pr_merged", event)
        if ok then
            return true, true
        end
        log.warn("GitHub: failed to emit pr_merged event: " .. tostring(err))
    end
    return true, false
end

local function is_pr_lifecycle_message(message, payload)
    local event_type = tostring(message.event_type or payload.event_type or "")
    return event_type == "pull_request"
        or event_type == "pull_request_review"
        or event_type:find("^pr_") ~= nil
        or payload.pull_request ~= nil
        or payload.pr_number ~= nil
end

local function handle_message(default_repo, message, channel_id)
    local payload = message.payload or {}
    local event_repo = message.repo or default_repo
    local pr_lifecycle_event = emit_pr_merged(event_repo, message, payload)

    if pr_lifecycle_event and is_pr_lifecycle_message(message, payload) and not (payload.prompt or payload.context or payload.comment_body) then
        action_cable.perform(channel_id, "ack", { id = message.id })
        return
    end

    if message.event_type == "agent_cleanup" then
        if payload.issue_number then
            local ws_name = github_workspace_name(event_repo, payload.issue_number)
            local matches = Agent.find_by_workspace(ws_name)
            for _, agent in ipairs(matches) do
                Hub.get():delete_agent(agent.session_uuid, false)
            end
        end
    else
        local existing = find_matching_agent(event_repo, payload)
        if existing then
            notify_agent(existing, payload)
        else
            local issue_num = payload.issue_number
            local ws_name = github_workspace_name(event_repo, issue_num)
            Hub.get():create_agent({
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

local function normalize_repos(repos)
    if type(repos) == "string" then
        repos = { repos }
    end
    local out = {}
    local seen = {}
    for _, repo in ipairs(repos or {}) do
        if type(repo) == "string" and repo ~= "" and not seen[repo] then
            seen[repo] = true
            out[#out + 1] = repo
        end
    end
    return out
end

function M.start(repos)
    M.stop()

    local normalized_repos = normalize_repos(repos)
    if #normalized_repos == 0 then
        return
    end

    routing_state.conn = action_cable.connect()
    routing_state.routes = {}
    for _, repo in ipairs(normalized_repos) do
        local channel = action_cable.subscribe(
            routing_state.conn,
            "Github::EventsChannel",
            { repo = repo },
            function(message, channel_id)
                handle_message(repo, message, channel_id)
            end
        )
        routing_state.routes[#routing_state.routes + 1] = {
            repo = repo,
            channel = channel,
        }
    end
end

function M.stop()
    if routing_state.conn then
        action_cable.close(routing_state.conn)
        routing_state.conn = nil
    end
    routing_state.routes = nil
    routing_state.channel = nil
end

return M
