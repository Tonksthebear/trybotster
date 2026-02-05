-- Agent lifecycle handler (hot-reloadable)
--
-- Orchestrates agent creation and deletion with full lifecycle broadcasting.
--
-- Responsibilities:
-- - Parse issue-or-branch input into branch_name + issue_number
-- - Find or create worktrees
-- - Spawn agents via Agent.new() (which handles PTY, env, prompt files)
-- - Broadcast agent lifecycle events to connected clients
--
-- Lifecycle stages (broadcast to all clients):
-- - creating_worktree: Worktree creation started
-- - spawning_ptys: PTY session spawning started
-- - running: Agent fully operational
-- - stopping: Agent shutdown initiated
-- - removing_worktree: Worktree deletion queued
-- - deleted: Agent fully cleaned up
--
-- The Agent class (lib.agent) does the heavy lifting. This handler is
-- the orchestration layer that connects incoming requests to the Agent API.

local Agent = require("lib.agent")
local connections = require("handlers.connections")

-- ============================================================================
-- Input Parsing
-- ============================================================================

--- Parse an issue-or-branch string into structured fields.
-- If the input is a bare number, treat it as an issue number and derive
-- the branch name. Otherwise treat it as a literal branch name.
--
-- @param issue_or_branch string  Issue number or branch name
-- @return issue_number number|nil
-- @return branch_name string
local function parse_issue_or_branch(issue_or_branch)
    local issue_number = tonumber(issue_or_branch)
    if issue_number then
        return issue_number, "botster-issue-" .. issue_number
    else
        return nil, issue_or_branch
    end
end

--- Generate a default prompt when none is provided.
--
-- @param issue_number number|nil
-- @param branch_name string
-- @return string
local function default_prompt(issue_number, branch_name)
    if issue_number then
        return string.format("Work on issue #%d", issue_number)
    else
        return string.format("Work on %s", branch_name)
    end
end

--- Build the agent key for duplicate checking.
-- Matches Agent:agent_key() format: repo with "/" replaced by "-", plus
-- issue_number or branch_name.
--
-- @param repo string  "owner/repo"
-- @param issue_number number|nil
-- @param branch_name string
-- @return string
local function build_agent_key(repo, issue_number, branch_name)
    local repo_safe = repo:gsub("/", "-")
    if issue_number then
        return repo_safe .. "-" .. tostring(issue_number)
    else
        local branch_safe = branch_name:gsub("/", "-")
        return repo_safe .. "-" .. branch_safe
    end
end

-- ============================================================================
-- Lifecycle Broadcasting
-- ============================================================================

--- Broadcast a lifecycle status change for a pending agent.
-- Used during creation before the agent object exists.
--
-- @param agent_key string The agent key
-- @param status string The lifecycle status
-- @param extra table|nil Optional extra fields to include
local function broadcast_lifecycle_status(agent_key, status, extra)
    local payload = {
        agent_id = agent_key,
        status = status,
    }
    if extra then
        for k, v in pairs(extra) do
            payload[k] = v
        end
    end
    connections.broadcast_hub_event("agent_status_changed", payload)
end

--- Broadcast the current agent list to all connected clients.
-- Uses connections.broadcast_hub_event to send to all hub-subscribed clients.
local function broadcast_agent_list()
    connections.broadcast_hub_event("agent_list", {
        agents = Agent.all_info(),
    })
end

--- Broadcast the current worktree list to all connected clients.
-- Called after agent creation/deletion since worktree availability changes.
local function broadcast_worktree_list()
    local worktrees = hub.get_worktrees()
    connections.broadcast_hub_event("worktree_list", {
        worktrees = worktrees,
    })
end

-- ============================================================================
-- Agent Spawning (internal)
-- ============================================================================

--- Spawn an agent in an existing worktree.
--
-- @param branch_name string
-- @param issue_number number|nil
-- @param wt_path string        Worktree filesystem path
-- @param prompt string          Task description
-- @param client table|nil       Requesting client (for dimensions)
-- @param agent_key string       Pre-computed agent key for status broadcasts
-- @return Agent|nil             The created agent, or nil on error
local function spawn_agent(branch_name, issue_number, wt_path, prompt, client, agent_key)
    local repo = os.getenv("BOTSTER_REPO") or "unknown/repo"

    -- Broadcast: spawning PTYs
    broadcast_lifecycle_status(agent_key, "spawning_ptys")

    -- Determine session config based on worktree contents
    local sessions = Agent.default_sessions()
    if fs.exists(wt_path .. "/.botster_server") then
        sessions = Agent.default_sessions_with_server()
    end

    -- Get dimensions from requesting client if available
    local dims = nil
    if client then
        dims = { rows = client.rows or 24, cols = client.cols or 80 }
    end

    local ok, agent = pcall(Agent.new, {
        repo = repo,
        issue_number = issue_number,
        branch_name = branch_name,
        worktree_path = wt_path,
        prompt = prompt,
        sessions = sessions,
        dims = dims,
    })

    if not ok then
        log.error(string.format("Failed to spawn agent for %s: %s",
            branch_name, tostring(agent)))
        -- Broadcast failure status
        broadcast_lifecycle_status(agent_key, "failed", { error = tostring(agent) })
        return nil
    end

    -- Emit agent_created event (triggers broadcast in connections.lua)
    events.emit("agent_created", agent:info())

    -- Broadcast updated lists to all clients
    broadcast_agent_list()
    broadcast_worktree_list()

    return agent
end

-- ============================================================================
-- Public API
-- ============================================================================

--- Handle a request to create a new agent.
-- Called by Client:on_message when it receives a "create_agent" subscription
-- message, or by command channel processing.
--
-- Supports two launch modes:
-- 1. Main repo mode: No issue_or_branch AND no from_worktree - agent runs in repo root
-- 2. Worktree mode: Find existing or create new worktree, then spawn agent
--
-- @param issue_or_branch string|nil  Issue number or branch name (nil for main repo mode)
-- @param prompt string|nil           Optional task prompt
-- @param from_worktree string|nil    Optional existing worktree path
-- @param client table|nil            Requesting client (for progress/dims)
-- @return Agent|nil                  The created agent, or nil on error
local function handle_create_agent(issue_or_branch, prompt, from_worktree, client)
    -- Main repo mode: no issue_or_branch AND no from_worktree
    if not issue_or_branch and not from_worktree then
        local repo_root = worktree.repo_root()
        if not repo_root then
            log.error("No issue_or_branch and no repo root detected")
            return nil
        end
        local repo = os.getenv("BOTSTER_REPO") or "unknown/repo"
        local agent_key = build_agent_key(repo, nil, "main")
        return spawn_agent("main", nil, repo_root, prompt or "Work on the main branch", client, agent_key)
    end

    local issue_number, branch_name = parse_issue_or_branch(issue_or_branch)

    -- Generate default prompt if not provided
    if not prompt or prompt == "" then
        prompt = default_prompt(issue_number, branch_name)
    end

    -- Detect repo
    local repo = os.getenv("BOTSTER_REPO") or "unknown/repo"

    -- Build agent key for status broadcasts and duplicate checking
    local agent_key = build_agent_key(repo, issue_number, branch_name)

    -- Check for existing agent with this key
    local existing = Agent.get(agent_key)
    if existing then
        log.info("Agent already exists: " .. agent_key)
        return existing
    end

    -- Find or create worktree
    local wt_path = from_worktree or worktree.find(branch_name)
    if not wt_path then
        -- Broadcast: creating worktree
        broadcast_lifecycle_status(agent_key, "creating_worktree")
        log.info(string.format("No worktree found for %s, creating...", branch_name))

        local ok, result = pcall(worktree.create, branch_name)
        if ok then
            wt_path = result
            log.info(string.format("Created worktree for %s at %s", branch_name, wt_path))
        else
            log.error(string.format("Failed to create worktree for %s: %s", branch_name, tostring(result)))
            broadcast_lifecycle_status(agent_key, "failed", { error = tostring(result) })
            return nil
        end
    else
        log.info(string.format("Worktree found for %s at %s", branch_name, wt_path))
    end

    return spawn_agent(branch_name, issue_number, wt_path, prompt, client, agent_key)
end

--- Handle a request to delete an agent.
--
-- @param agent_key string       Agent key (repo-issue or repo-branch)
-- @param delete_worktree boolean  Whether to also delete the worktree
-- @return boolean                 True if agent was found and deleted
local function handle_delete_agent(agent_key, delete_worktree)
    local agent = Agent.get(agent_key)
    if not agent then
        log.warn("Cannot delete unknown agent: " .. tostring(agent_key))
        return false
    end

    -- Broadcast: stopping
    broadcast_lifecycle_status(agent_key, "stopping")

    -- Close the agent (kills PTY sessions)
    agent:close(delete_worktree)

    -- Broadcast: deleted (or removing_worktree if that was requested)
    if delete_worktree then
        broadcast_lifecycle_status(agent_key, "removing_worktree")
    end

    -- Emit agent_deleted event
    events.emit("agent_deleted", agent_key)

    -- Broadcast updated lists to all clients
    broadcast_agent_list()
    broadcast_worktree_list()

    return true
end

-- ============================================================================
-- Event Listeners
-- ============================================================================

-- Handle command channel messages that create or delete agents.
events.on("command_message", function(message)
    if not message then return end

    local msg_type = message.type or message.command
    if msg_type == "create_agent" then
        local issue_or_branch = message.issue_or_branch or message.branch
        if issue_or_branch then
            handle_create_agent(issue_or_branch, message.prompt, message.from_worktree)
        else
            log.warn("command_message create_agent missing issue_or_branch")
        end
    elseif msg_type == "delete_agent" then
        local agent_id = message.id or message.agent_id or message.session_key
        if agent_id then
            handle_delete_agent(agent_id, message.delete_worktree or false)
        else
            log.warn("command_message delete_agent missing agent_id")
        end
    end
end)

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {
    handle_create_agent = handle_create_agent,
    handle_delete_agent = handle_delete_agent,
    broadcast_agent_list = broadcast_agent_list,
    broadcast_worktree_list = broadcast_worktree_list,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("agents.lua reloading")
end

function M._after_reload()
    log.info(string.format("agents.lua reloaded (%d agents active)", Agent.count()))
end

log.info(string.format("Agent handler loaded (%d agents active)", Agent.count()))

return M
