-- Agent lifecycle handler (hot-reloadable)
--
-- Orchestrates agent creation and deletion with full lifecycle broadcasting.
--
-- Responsibilities:
-- - Parse issue-or-branch input into branch_name + issue_number
-- - Find or create worktrees
-- - Resolve config profiles via ConfigResolver
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
local ConfigResolver = require("lib.config_resolver")

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
-- Profile Resolution
-- ============================================================================

--- Resolve profile name from user input.
--
-- Three input cases:
--   non-empty string  → explicit profile name, use as-is
--   empty string ""   → user explicitly chose "Default" (shared-only)
--   nil               → not specified, auto-select or fall back to shared
--
-- @param repo_root string Repository root path
-- @param profile_name string|nil Input from user/browser
-- @return string|nil Resolved profile name (nil = shared-only)
-- @return string|nil Error message if resolution fails
local function resolve_profile_name(repo_root, profile_name)
    -- Explicit profile name
    if profile_name and profile_name ~= "" then
        return profile_name, nil
    end

    -- Empty string = user chose "Default" (shared-only)
    if profile_name == "" then
        if ConfigResolver.has_shared_agent(repo_root) then
            return nil, nil
        end
        return nil, "Cannot use Default: no agent session in shared/"
    end

    -- nil = not specified, auto-select
    local profiles = ConfigResolver.list_profiles(repo_root)
    if #profiles == 0 then
        if ConfigResolver.has_shared_agent(repo_root) then
            log.info("No profiles found, using shared-only config")
            return nil, nil
        end
        return nil, "No profiles found and no shared agent session."
    elseif #profiles == 1 then
        log.info(string.format("Auto-selected profile: %s", profiles[1]))
        return profiles[1], nil
    else
        return nil, string.format(
            "Multiple profiles available (%s). Please specify a profile.",
            table.concat(profiles, ", "))
    end
end

--- Build session configs for Agent.new() from resolved config.
-- Maps ConfigResolver output to the format Agent.new() expects.
-- @param resolved table ConfigResolver.resolve() output
-- @return array Session configs for Agent.new()
local function build_sessions_from_resolved(resolved)
    local sessions = {}
    for _, session in ipairs(resolved.sessions) do
        sessions[#sessions + 1] = {
            name = session.name,
            command = "bash",
            init_script = session.initialization,  -- absolute path
            notifications = (session.name == "agent"),
            forward_port = session.port_forward,
        }
    end
    return sessions
end

-- ============================================================================
-- Lifecycle Broadcasting
-- ============================================================================

--- Notify lifecycle status change via hooks.
-- Used during creation/deletion for intermediate statuses (creating_worktree,
-- spawning_ptys, stopping, etc.). Observers in connections.lua broadcast to clients.
--
-- @param agent_key string The agent key
-- @param status string The lifecycle status
-- @param extra table|nil Optional extra fields to include
local function notify_lifecycle(agent_key, status, extra)
    local payload = {
        agent_id = agent_key,
        status = status,
    }
    if extra then
        for k, v in pairs(extra) do
            payload[k] = v
        end
    end
    hooks.notify("agent_lifecycle", payload)
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
-- @param profile_name string    Profile to use for config resolution
-- @return Agent|nil             The created agent, or nil on error
local function spawn_agent(branch_name, issue_number, wt_path, prompt, client, agent_key, profile_name)
    local repo = config.env("BOTSTER_REPO") or "unknown/repo"
    local repo_root = worktree.repo_root()

    -- Broadcast: spawning PTYs
    notify_lifecycle(agent_key, "spawning_ptys")

    -- Resolve config from .botster/ directory
    local resolved, err = ConfigResolver.resolve(repo_root, profile_name)
    if not resolved then
        log.error(string.format("Config resolution failed for profile '%s': %s",
            tostring(profile_name), tostring(err)))
        notify_lifecycle(agent_key, "failed", { error = tostring(err) })
        return nil
    end

    local sessions = build_sessions_from_resolved(resolved)

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
        notify_lifecycle(agent_key, "failed", { error = tostring(agent) })
        return nil
    end

    -- Notify via hooks (connections.lua observes and broadcasts to clients)
    hooks.notify("agent_created", agent:info())

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
-- @param profile_name string|nil     Profile name (auto-selected if only one)
-- @return Agent|nil                  The created agent, or nil on error
local function handle_create_agent(issue_or_branch, prompt, from_worktree, client, profile_name)
    -- Interceptor: plugins can transform params or block creation (return nil)
    local params = hooks.call("before_agent_create", {
        issue_or_branch = issue_or_branch,
        prompt = prompt,
        from_worktree = from_worktree,
        profile_name = profile_name,
    })
    if params == nil then
        log.info("before_agent_create interceptor blocked agent creation")
        return nil
    end
    -- Allow interceptors to modify fields
    issue_or_branch = params.issue_or_branch
    prompt = params.prompt
    from_worktree = params.from_worktree
    profile_name = params.profile_name

    -- Resolve profile name (auto-select if only one, nil = shared-only)
    local repo_root = worktree.repo_root()
    if repo_root then
        local resolved_profile, profile_err = resolve_profile_name(repo_root, profile_name)
        if profile_err then
            log.error(string.format("Profile resolution failed: %s", profile_err))
            return nil
        end
        profile_name = resolved_profile  -- nil for shared-only, or a profile name
    else
        log.error("Cannot resolve profile: no repo root detected")
        return nil
    end

    -- Main repo mode: no issue_or_branch AND no from_worktree
    if not issue_or_branch and not from_worktree then
        local repo = config.env("BOTSTER_REPO") or "unknown/repo"
        local agent_key = build_agent_key(repo, nil, "main")
        return spawn_agent("main", nil, repo_root, prompt or "Work on the main branch", client, agent_key, profile_name)
    end

    local issue_number, branch_name = parse_issue_or_branch(issue_or_branch)

    -- Generate default prompt if not provided
    if not prompt or prompt == "" then
        prompt = default_prompt(issue_number, branch_name)
    end

    -- Detect repo
    local repo = config.env("BOTSTER_REPO") or "unknown/repo"

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
        notify_lifecycle(agent_key, "creating_worktree")
        log.info(string.format("No worktree found for %s, creating...", branch_name))

        local ok, result = pcall(worktree.create, branch_name)
        if ok then
            wt_path = result
            log.info(string.format("Created worktree for %s at %s", branch_name, wt_path))

            -- Copy workspace files into new worktree
            local resolved_for_copy, _ = ConfigResolver.resolve(repo_root, profile_name)
            if resolved_for_copy and resolved_for_copy.workspace_include then
                local ok_copy, copy_err = pcall(worktree.copy_from_patterns,
                    repo_root, wt_path, resolved_for_copy.workspace_include)
                if not ok_copy then
                    log.warn(string.format("Failed to copy workspace files: %s", tostring(copy_err)))
                end
            end
        else
            log.error(string.format("Failed to create worktree for %s: %s", branch_name, tostring(result)))
            notify_lifecycle(agent_key, "failed", { error = tostring(result) })
            return nil
        end
    else
        log.info(string.format("Worktree found for %s at %s", branch_name, wt_path))
    end

    return spawn_agent(branch_name, issue_number, wt_path, prompt, client, agent_key, profile_name)
end

--- Handle a request to delete an agent.
--
-- @param agent_key string       Agent key (repo-issue or repo-branch)
-- @param delete_worktree boolean  Whether to also delete the worktree
-- @return boolean                 True if agent was found and deleted
local function handle_delete_agent(agent_key, delete_worktree)
    -- Interceptor: plugins can block deletion (return nil)
    local config = hooks.call("before_agent_delete", {
        agent_key = agent_key,
        delete_worktree = delete_worktree,
    })
    if config == nil then
        log.info("before_agent_delete interceptor blocked agent deletion")
        return false
    end
    -- Allow interceptors to modify fields
    agent_key = config.agent_key
    delete_worktree = config.delete_worktree

    local agent = Agent.get(agent_key)
    if not agent then
        log.warn("Cannot delete unknown agent: " .. tostring(agent_key))
        return false
    end

    -- Broadcast: stopping
    notify_lifecycle(agent_key, "stopping")

    -- Close the agent (kills PTY sessions)
    agent:close(delete_worktree)

    -- Broadcast: deleted (or removing_worktree if that was requested)
    if delete_worktree then
        notify_lifecycle(agent_key, "removing_worktree")
    end

    -- Notify via hooks (connections.lua observes and broadcasts to clients)
    hooks.notify("agent_deleted", agent_key)

    return true
end

-- ============================================================================
-- Event Listeners
-- ============================================================================

--- Format a notification string for an existing agent.
-- Matches the format used by the Rust try_notify_existing_agent().
-- @param message table The command_message with prompt/context fields
-- @return string The notification text
local function format_notification(message)
    local prompt = message.prompt
    if prompt then
        return string.format(
            "=== NEW MENTION (automated notification) ===\n\n%s\n\n==================",
            prompt
        )
    else
        return "=== NEW MENTION (automated notification) ===\nNew mention\n=================="
    end
end

--- Notify an existing agent of a new mention via PTY input.
-- Writes the notification text to the agent's "agent" session PTY.
-- @param agent Agent The existing agent to notify
-- @param text string The notification text
local function notify_existing_agent(agent, text)
    local session = agent.sessions and agent.sessions["agent"]
    if session then
        session:write(text .. "\r\r")
        log.info("Sent notification to existing agent: " .. agent:agent_key())
    else
        log.warn("Cannot notify agent (no 'agent' session): " .. agent:agent_key())
    end
end

-- Handle command channel messages that create or delete agents.
events.on("command_message", function(message)
    if not message then return end

    local msg_type = message.type or message.command
    if msg_type == "create_agent" then
        local issue_or_branch = message.issue_or_branch or message.branch

        -- Check if an agent already exists for this issue — notify instead of creating
        if issue_or_branch then
            local repo = message.repo or config.env("BOTSTER_REPO") or "unknown/repo"
            local issue_number, branch_name = parse_issue_or_branch(issue_or_branch)
            local agent_key = build_agent_key(repo, issue_number, branch_name)
            local existing = Agent.get(agent_key)
            if existing then
                log.info("Agent exists for " .. agent_key .. ", sending notification")
                notify_existing_agent(existing, format_notification(message))
                return
            end
        end

        if issue_or_branch then
            handle_create_agent(issue_or_branch, message.prompt, message.from_worktree, nil, message.profile)
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
