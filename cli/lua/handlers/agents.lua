-- Agent lifecycle handler (hot-reloadable)
--
-- Orchestrates agent creation and deletion with full lifecycle broadcasting.
--
-- Responsibilities:
-- - Parse issue-or-branch input into branch_name
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

--- Build the agent key for duplicate checking.
-- Matches Agent:agent_key() format: repo with "/" replaced by "-", plus branch_name.
--
-- @param repo string  "owner/repo"
-- @param branch_name string
-- @return string
local function build_agent_key(repo, branch_name)
    local repo_safe = repo:gsub("/", "-")
    local branch_safe = branch_name:gsub("/", "-")
    return repo_safe .. "-" .. branch_safe
end

--- Find the next available agent key by appending a suffix if needed.
-- If base_key is free, returns it as-is. Otherwise tries base_key-2, -3, etc.
--
-- @param base_key string  The base agent key
-- @return string          An unused agent key
local function next_available_key(base_key)
    if not Agent.get(base_key) then
        return base_key
    end
    local i = 2
    while Agent.get(base_key .. "-" .. i) do
        i = i + 1
    end
    return base_key .. "-" .. i
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
    local device_root = config.data_dir and config.data_dir() or nil

    -- Explicit profile name
    if profile_name and profile_name ~= "" then
        return profile_name, nil
    end

    -- Empty string = user chose "Default" (shared-only)
    if profile_name == "" then
        if ConfigResolver.has_agent_without_profile(device_root, repo_root) then
            return nil, nil
        end
        return nil, "Cannot use Default: no agent session in shared/"
    end

    -- nil = not specified, auto-select
    local profiles = ConfigResolver.list_profiles_all(device_root, repo_root)
    if #profiles == 0 then
        if ConfigResolver.has_agent_without_profile(device_root, repo_root) then
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
-- @param resolved table ConfigResolver.resolve_all() output
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
-- @param wt_path string        Worktree filesystem path
-- @param prompt string          Task description
-- @param client table|nil       Requesting client (for dimensions)
-- @param agent_key string       Pre-computed agent key for status broadcasts
-- @param profile_name string    Profile to use for config resolution
-- @param metadata table|nil     Plugin metadata (e.g., issue_number, invocation_url)
-- @return Agent|nil             The created agent, or nil on error
local function spawn_agent(branch_name, wt_path, prompt, client, agent_key, profile_name, metadata)
    local repo = config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
    local repo_root = worktree.repo_root()

    -- Broadcast: spawning PTYs
    notify_lifecycle(agent_key, "spawning_ptys")

    -- Resolve config across device + repo layers
    local device_root = config.data_dir and config.data_dir() or nil
    local resolved, err = ConfigResolver.resolve_all({
        device_root = device_root,
        repo_root = repo_root,
        profile = profile_name,
    })
    if not resolved then
        log.error(string.format("Config resolution failed for profile '%s': %s",
            tostring(profile_name), tostring(err)))
        notify_lifecycle(agent_key, "failed", { error = tostring(err) })
        return nil
    end

    local sessions = build_sessions_from_resolved(resolved)

    -- Default dimensions for PTY creation. The actual client dimensions
    -- are set when the client subscribes to the terminal channel via pty_clients.
    local dims = { rows = 24, cols = 80 }

    local ok, agent = pcall(Agent.new, {
        repo = repo,
        branch_name = branch_name,
        worktree_path = wt_path,
        prompt = prompt,
        metadata = metadata,
        sessions = sessions,
        dims = dims,
        agent_key = agent_key,
        profile_name = profile_name,
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
-- @param metadata table|nil          Plugin metadata (e.g., issue_number, invocation_url)
-- @return Agent|nil                  The created agent, or nil on error
local function handle_create_agent(issue_or_branch, prompt, from_worktree, client, profile_name, metadata)
    -- Early identifier for lifecycle events on error paths (matches what TUI
    -- sets for creating_agent_id in actions.lua).
    local early_id = issue_or_branch or "main"

    -- Interceptor: plugins can transform params or block creation (return nil)
    local params = hooks.call("before_agent_create", {
        issue_or_branch = issue_or_branch,
        prompt = prompt,
        from_worktree = from_worktree,
        profile_name = profile_name,
        metadata = metadata,
    })
    if params == nil then
        log.info("before_agent_create interceptor blocked agent creation")
        notify_lifecycle(early_id, "failed", { error = "Blocked by interceptor" })
        return nil
    end
    -- Allow interceptors to modify fields
    issue_or_branch = params.issue_or_branch
    prompt = params.prompt
    from_worktree = params.from_worktree
    profile_name = params.profile_name
    metadata = params.metadata

    -- Resolve profile name (auto-select if only one, nil = shared-only)
    local repo_root = worktree.repo_root()
    if repo_root then
        local resolved_profile, profile_err = resolve_profile_name(repo_root, profile_name)
        if profile_err then
            log.error(string.format("Profile resolution failed: %s", profile_err))
            notify_lifecycle(early_id, "failed", { error = profile_err })
            return nil
        end
        profile_name = resolved_profile  -- nil for shared-only, or a profile name
    else
        log.error("Cannot resolve profile: no repo root detected")
        notify_lifecycle(early_id, "failed", { error = "No repo root detected" })
        return nil
    end

    -- Main repo mode: no issue_or_branch AND no from_worktree
    if not issue_or_branch and not from_worktree then
        local repo = config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
        local base_key = build_agent_key(repo, "main")
        local suffix = Agent.next_instance_suffix(base_key)
        local agent_key = base_key .. (suffix or "")
        return spawn_agent("main", repo_root, prompt, client, agent_key, profile_name, metadata)
    end

    local _, branch_name = parse_issue_or_branch(issue_or_branch)

    -- Treat empty string as no prompt
    if prompt == "" then
        prompt = nil
    end

    -- Detect repo
    local repo = config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"

    -- Build agent key for status broadcasts and duplicate checking
    local agent_key = build_agent_key(repo, branch_name)

    -- Allow multiple agents on the same branch by suffixing the key
    agent_key = next_available_key(agent_key)

    -- Non-git mode: no worktree isolation, spawn directly in cwd
    if not worktree.is_git_repo() then
        log.info(string.format("No git repo — spawning %s directly in %s", branch_name, repo_root))
        return spawn_agent(branch_name, repo_root, prompt, client, agent_key, profile_name, metadata)
    end

    -- Find or create worktree
    local wt_path = from_worktree or worktree.find(branch_name)

    -- worktree.find() only checks linked worktrees, not the main checkout.
    -- hub.get_worktrees() also excludes it (filters by .git file vs directory).
    -- Read .git/HEAD directly to check if branch_name is the main repo branch.
    if not wt_path then
        local head_path = repo_root .. "/.git/HEAD"
        local f = io.open(head_path, "r")
        if f then
            local head = f:read("*l")
            f:close()
            -- HEAD contains "ref: refs/heads/<branch>" when on a branch
            local main_branch = head and head:match("^ref: refs/heads/(.+)$")
            if main_branch == branch_name then
                wt_path = repo_root
                log.info(string.format("Branch '%s' is main repo checkout, using %s", branch_name, repo_root))
            end
        end
    end

    if not wt_path then
        -- Broadcast: creating worktree (sent immediately to clients)
        notify_lifecycle(agent_key, "creating_worktree")
        log.info(string.format("No worktree found for %s, queueing async creation...", branch_name))

        -- Queue async creation — returns immediately, Hub fires worktree_created
        -- or worktree_create_failed event when git completes on blocking thread.
        local client_rows = 24
        local client_cols = 80
        worktree.create_async({
            agent_key = agent_key,
            branch = branch_name,
            prompt = prompt,
            metadata = metadata,
            profile_name = profile_name,
            client_rows = client_rows,
            client_cols = client_cols,
        })
        return nil  -- Agent spawning continues in worktree_created event handler
    else
        log.info(string.format("Worktree found for %s at %s", branch_name, wt_path))
    end

    return spawn_agent(branch_name, wt_path, prompt, client, agent_key, profile_name, metadata)
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
        session:send_message(text)
        log.info("Sent notification to existing agent: " .. agent:agent_key())
    else
        log.warn("Cannot notify agent (no 'agent' session): " .. agent:agent_key())
    end
end

-- Track event subscriptions for cleanup on hot-reload
local _event_subs = {}

-- Handle command channel messages that create or delete agents.
_event_subs[#_event_subs + 1] = events.on("command_message", function(message)
    if not message then return end

    local msg_type = message.type or message.command
    if msg_type == "create_agent" then
        local issue_or_branch = message.issue_or_branch or message.branch

        -- Check if any agents already exist for this issue/repo — notify them
        if issue_or_branch then
            local repo = message.repo or config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
            local issue_number, _ = parse_issue_or_branch(issue_or_branch)

            -- Search by metadata (repo + issue_number)
            local existing = {}
            if issue_number then
                for _, agent in ipairs(Agent.find_by_meta("issue_number", issue_number)) do
                    if agent.repo == repo then
                        existing[#existing + 1] = agent
                    end
                end
            end

            if #existing > 0 then
                local notification = format_notification(message)
                for _, agent in ipairs(existing) do
                    log.info("Agent exists for " .. agent:agent_key() .. ", sending notification")
                    notify_existing_agent(agent, notification)
                end
                return
            end
        end

        if issue_or_branch then
            -- Build metadata from message fields
            local meta = message.metadata or {}
            if message.invocation_url and not meta.invocation_url then
                meta.invocation_url = message.invocation_url
            end
            local issue_number, _ = parse_issue_or_branch(issue_or_branch)
            if issue_number and not meta.issue_number then
                meta.issue_number = issue_number
            end
            handle_create_agent(issue_or_branch, message.prompt, message.from_worktree, nil, message.profile, meta)
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
-- Async Worktree Creation Callbacks
-- ============================================================================

--- Resume agent spawning after async worktree creation completes.
-- Fired by Hub when spawn_blocking finishes the git worktree add.
-- Carries all context needed to continue where handle_create_agent left off.
_event_subs[#_event_subs + 1] = events.on("worktree_created", function(info)
    log.info(string.format("Worktree created for %s at %s, resuming agent spawn",
        info.branch, info.path))

    local repo_root = worktree.repo_root()

    -- Copy workspace files into new worktree (same logic as was in handle_create_agent)
    local device_root_copy = config.data_dir and config.data_dir() or nil
    local resolved_for_copy, _ = ConfigResolver.resolve_all({
        device_root = device_root_copy,
        repo_root = repo_root,
        profile = info.profile_name,
    })
    if resolved_for_copy and resolved_for_copy.workspace_include then
        local ok_copy, copy_err = pcall(worktree.copy_from_patterns,
            repo_root, info.path, resolved_for_copy.workspace_include.path)
        if not ok_copy then
            log.warn(string.format("Failed to copy workspace files: %s", tostring(copy_err)))
        end
    end

    -- Reconstruct a minimal client table for dimensions
    local client = { rows = info.client_rows, cols = info.client_cols }

    spawn_agent(
        info.branch,
        info.path,
        info.prompt,
        client,
        info.agent_key,
        info.profile_name,
        info.metadata
    )
end)

--- Handle async worktree creation failure.
-- Fired by Hub when the blocking git operation fails.
_event_subs[#_event_subs + 1] = events.on("worktree_create_failed", function(info)
    log.error(string.format("Async worktree creation failed for %s: %s",
        info.branch, info.error))
    notify_lifecycle(info.agent_key, "failed", { error = info.error })
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
    -- Unsubscribe event listeners to prevent duplicate firing
    for _, sub_id in ipairs(_event_subs) do
        events.off(sub_id)
    end
    _event_subs = {}
    log.info("agents.lua reloading")
end

function M._after_reload()
    log.info(string.format("agents.lua reloaded (%d agents active)", Agent.count()))
end

log.info(string.format("Agent handler loaded (%d agents active)", Agent.count()))

return M
