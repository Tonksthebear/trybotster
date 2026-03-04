-- Agent lifecycle handler (hot-reloadable)
--
-- Orchestrates agent and accessory creation/deletion with full lifecycle broadcasting.
--
-- Responsibilities:
-- - Parse issue-or-branch input into branch_name
-- - Find or create worktrees
-- - Resolve config profiles via ConfigResolver
-- - Spawn agents (single PTY) via Agent.new()
-- - Spawn accessories (single PTY, no AI autonomy) via Agent.new()
-- - Broadcast agent lifecycle events to connected clients
--
-- Single-PTY model: each Agent instance has exactly one PTY.
-- Agents have AI autonomy. Accessories are plain PTY sessions.
-- Session UUID is the primary key for everything.

local Agent = require("lib.agent")
local ConfigResolver = require("lib.config_resolver")

-- ============================================================================
-- Input Parsing
-- ============================================================================

--- Parse an issue-or-branch string into structured fields.
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
-- @param repo string  "owner/repo"
-- @param branch_name string
-- @return string
local function build_agent_key(repo, branch_name)
    local repo_safe = repo:gsub("/", "-")
    local branch_safe = branch_name:gsub("/", "-")
    return repo_safe .. "-" .. branch_safe
end

--- Find the next available agent key by appending a suffix if needed.
-- Uses Agent.find_by_agent_key to check for existing agents by key.
-- @param base_key string  The base agent key
-- @return string          An unused agent key
local function next_available_key(base_key)
    if not Agent.find_by_agent_key(base_key) then
        return base_key
    end
    local i = 2
    while Agent.find_by_agent_key(base_key .. "-" .. i) do
        i = i + 1
    end
    return base_key .. "-" .. i
end

-- ============================================================================
-- Profile Resolution
-- ============================================================================

--- Resolve profile name from user input.
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

--- Pick the "agent" session config from resolved config.
-- Returns a single session config for the primary agent PTY.
-- @param resolved table ConfigResolver.resolve_all() output
-- @return table Single session config for Agent.new()
local function pick_agent_session(resolved)
    for _, session in ipairs(resolved.sessions) do
        if session.name == "agent" then
            return {
                name = "agent",
                command = "bash",
                init_script = session.initialization,
                notifications = true,
                forward_port = session.port_forward,
            }
        end
    end
    -- Fallback: use first session
    local session = resolved.sessions[1]
    return {
        name = session.name,
        command = "bash",
        init_script = session.initialization,
        notifications = (session.name == "agent"),
        forward_port = session.port_forward,
    }
end

--- Pick a named session config from resolved config for an accessory.
-- @param resolved table ConfigResolver.resolve_all() output
-- @param session_name string Name of the session to pick
-- @return table|nil Single session config, or nil if not found
local function pick_named_session(resolved, session_name)
    for _, session in ipairs(resolved.sessions) do
        if session.name == session_name then
            return {
                name = session_name,
                command = "bash",
                init_script = session.initialization,
                notifications = false,
                forward_port = session.port_forward,
            }
        end
    end
    return nil
end

-- ============================================================================
-- Lifecycle Broadcasting
-- ============================================================================

--- Notify lifecycle status change via hooks.
-- @param agent_id string The agent key or session_uuid
-- @param status string The lifecycle status
-- @param extra table|nil Optional extra fields to include
local function notify_lifecycle(agent_id, status, extra)
    local payload = {
        agent_id = agent_id,
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
-- @param metadata table|nil     Plugin metadata
-- @return Agent|nil             The created agent, or nil on error
-- @return string|nil            Error message (nil on success)
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
        local msg = string.format("Config resolution failed for profile '%s': %s",
            tostring(profile_name), tostring(err))
        log.error(msg)
        notify_lifecycle(agent_key, "failed", { error = tostring(err) })
        return nil, msg
    end

    -- Pick the single "agent" session from resolved config
    local session_config = pick_agent_session(resolved)

    -- Default dimensions
    local dims = { rows = 24, cols = 80 }

    -- Extract workspace fields from metadata; default to branch name so agents
    -- on the same branch share a workspace.
    local workspace_name = metadata and metadata.workspace or branch_name
    local workspace_id = metadata and metadata.workspace_id or nil
    local workspace_metadata = metadata and metadata.workspace_metadata or nil

    local ok, agent = pcall(Agent.new, {
        repo = repo,
        branch_name = branch_name,
        worktree_path = wt_path,
        prompt = prompt,
        metadata = metadata,
        workspace = workspace_name,
        workspace_id = workspace_id,
        workspace_metadata = workspace_metadata,
        session_type = "agent",
        session = session_config,
        dims = dims,
        agent_key = agent_key,
        profile_name = profile_name,
    })

    if not ok then
        local msg = string.format("Failed to spawn agent for %s: %s",
            branch_name, tostring(agent))
        log.error(msg)
        notify_lifecycle(agent_key, "failed", { error = tostring(agent) })
        return nil, msg
    end

    -- Notify via hooks (connections.lua observes and broadcasts to clients)
    hooks.notify("agent_created", agent:info())

    -- Deliver initial prompt to the agent PTY
    if prompt and prompt ~= "" and agent.session then
        agent.session:send_message(prompt)
    end

    return agent
end

--- Spawn an accessory in an existing worktree.
--
-- @param branch_name string
-- @param wt_path string        Worktree filesystem path
-- @param session_name string    Session name from config (e.g., "server")
-- @param agent_key string       Pre-computed agent key
-- @param profile_name string    Profile to use
-- @param metadata table|nil     Plugin metadata
-- @return Agent|nil
-- @return string|nil
local function spawn_accessory(branch_name, wt_path, session_name, agent_key, profile_name, metadata)
    local repo = config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
    local repo_root = worktree.repo_root()

    local device_root = config.data_dir and config.data_dir() or nil
    local resolved, err = ConfigResolver.resolve_all({
        device_root = device_root,
        repo_root = repo_root,
        profile = profile_name,
    })
    if not resolved then
        log.error(string.format("Config resolution failed: %s", tostring(err)))
        return nil, tostring(err)
    end

    local session_config = pick_named_session(resolved, session_name)
    if not session_config then
        -- Fall back to a raw shell with the given name
        session_config = { name = session_name, command = "bash" }
    end

    local workspace_name = metadata and metadata.workspace or branch_name
    local workspace_id = metadata and metadata.workspace_id or nil

    local ok, agent = pcall(Agent.new, {
        repo = repo,
        branch_name = branch_name,
        worktree_path = wt_path,
        session_type = "accessory",
        session = session_config,
        metadata = metadata,
        workspace = workspace_name,
        workspace_id = workspace_id,
        dims = { rows = 24, cols = 80 },
        agent_key = agent_key,
        profile_name = profile_name,
    })

    if not ok then
        log.error(string.format("Failed to spawn accessory: %s", tostring(agent)))
        return nil, tostring(agent)
    end

    hooks.notify("agent_created", agent:info())
    return agent
end

-- ============================================================================
-- Public API
-- ============================================================================

--- Handle a request to create a new agent.
-- @param issue_or_branch string|nil  Issue number or branch name
-- @param prompt string|nil           Optional task prompt
-- @param from_worktree string|nil    Optional existing worktree path
-- @param client table|nil            Requesting client
-- @param profile_name string|nil     Profile name
-- @param metadata table|nil          Plugin metadata
-- @return Agent|nil
-- @return string|nil
local function handle_create_agent(issue_or_branch, prompt, from_worktree, client, profile_name, metadata)
    local early_id = issue_or_branch or "main"

    -- Interceptor: plugins can transform params or block creation
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
        return nil, "Blocked by interceptor"
    end
    issue_or_branch = params.issue_or_branch
    prompt = params.prompt
    from_worktree = params.from_worktree
    profile_name = params.profile_name
    metadata = params.metadata

    -- Resolve profile name
    local repo_root = worktree.repo_root()
    if repo_root then
        local resolved_profile, profile_err = resolve_profile_name(repo_root, profile_name)
        if profile_err then
            log.error(string.format("Profile resolution failed: %s", profile_err))
            notify_lifecycle(early_id, "failed", { error = profile_err })
            return nil, "Profile resolution failed: " .. profile_err
        end
        profile_name = resolved_profile
    else
        log.error("Cannot resolve profile: no repo root detected")
        notify_lifecycle(early_id, "failed", { error = "No repo root detected" })
        return nil, "No repo root detected"
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

    if prompt == "" then
        prompt = nil
    end

    local repo = config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
    local agent_key = build_agent_key(repo, branch_name)
    agent_key = next_available_key(agent_key)

    -- Non-git mode
    if not worktree.is_git_repo() then
        log.info(string.format("No git repo — spawning %s directly in %s", branch_name, repo_root))
        return spawn_agent(branch_name, repo_root, prompt, client, agent_key, profile_name, metadata)
    end

    -- Find or create worktree
    local wt_path = from_worktree or worktree.find(branch_name)

    if not wt_path then
        local head_path = repo_root .. "/.git/HEAD"
        local f = io.open(head_path, "r")
        if f then
            local head = f:read("*l")
            f:close()
            local main_branch = head and head:match("^ref: refs/heads/(.+)$")
            if main_branch == branch_name then
                wt_path = repo_root
                log.info(string.format("Branch '%s' is main repo checkout, using %s", branch_name, repo_root))
            end
        end
    end

    if not wt_path then
        notify_lifecycle(agent_key, "creating_worktree")
        log.info(string.format("No worktree found for %s, queueing async creation...", branch_name))

        worktree.create_async({
            agent_key = agent_key,
            branch = branch_name,
            prompt = prompt,
            metadata = metadata,
            profile_name = profile_name,
            client_rows = 24,
            client_cols = 80,
        })
        return nil  -- Agent spawning continues in worktree_created event handler
    else
        log.info(string.format("Worktree found for %s at %s", branch_name, wt_path))
    end

    return spawn_agent(branch_name, wt_path, prompt, client, agent_key, profile_name, metadata)
end

--- Handle a request to create an accessory.
-- @param workspace string|nil     Workspace name (used to find worktree path)
-- @param session_name string      Session name from config (e.g., "server")
-- @param profile_name string|nil  Profile name
-- @param metadata table|nil       Plugin metadata
-- @return Agent|nil
-- @return string|nil
local function handle_create_accessory(workspace, session_name, profile_name, metadata)
    if not session_name then
        return nil, "session_name is required for accessories"
    end

    -- Find worktree from workspace or use repo root
    local wt_path = worktree.repo_root()
    local branch_name = "main"

    -- If workspace provided, try to find the worktree from existing agents
    if workspace then
        local existing = Agent.find_by_workspace(workspace)
        if #existing > 0 then
            wt_path = existing[1].worktree_path
            branch_name = existing[1].branch_name
        end
    end

    local repo = config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
    local base_key = build_agent_key(repo, branch_name) .. "-" .. session_name
    local agent_key = next_available_key(base_key)

    metadata = metadata or {}
    if workspace then
        metadata.workspace = workspace
    end

    return spawn_accessory(branch_name, wt_path, session_name, agent_key, profile_name, metadata)
end

--- Handle a request to delete a session (agent or accessory).
-- @param session_uuid string       Session UUID
-- @param delete_worktree boolean   Whether to also delete the worktree
-- @return boolean
local function handle_delete_session(session_uuid, delete_worktree)
    -- Interceptor: plugins can block deletion
    local cfg = hooks.call("before_agent_delete", {
        session_uuid = session_uuid,
        delete_worktree = delete_worktree,
    })
    if cfg == nil then
        log.info("before_agent_delete interceptor blocked deletion")
        return false
    end
    session_uuid = cfg.session_uuid
    delete_worktree = cfg.delete_worktree

    local agent = Agent.get(session_uuid)
    if not agent then
        -- Try lookup by agent_key for backward compat
        agent = Agent.find_by_agent_key(session_uuid)
        if not agent then
            log.warn("Cannot delete unknown session: " .. tostring(session_uuid))
            return false
        end
    end

    local agent_key = agent:agent_key()

    -- Broadcast: stopping
    notify_lifecycle(agent_key, "stopping")

    -- Guard: skip worktree deletion if other agents are still running in it
    if delete_worktree then
        local wt_path = agent.worktree_path
        local still_running = {}
        for _, other in ipairs(Agent.list()) do
            if other.session_uuid ~= agent.session_uuid and other.worktree_path == wt_path then
                still_running[#still_running + 1] = other:agent_key()
            end
        end
        if #still_running > 0 then
            log.warn(string.format(
                "Cannot delete worktree — session(s) [%s] still running in it",
                table.concat(still_running, ", ")))
            delete_worktree = false
        end
    end

    -- Close the agent (kills PTY session)
    agent:close(delete_worktree)

    if delete_worktree then
        notify_lifecycle(agent_key, "removing_worktree")
    end

    -- Notify via hooks
    hooks.notify("agent_deleted", agent_key)

    return true
end

-- Keep backward-compat name
local handle_delete_agent = handle_delete_session

-- ============================================================================
-- Event Listeners
-- ============================================================================

--- Format a notification string for an existing agent.
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
local function notify_existing_agent(agent, text)
    if agent.session then
        agent.session:send_message(text)
        log.info("Sent notification to existing agent: " .. agent:agent_key())
    else
        log.warn("Cannot notify agent (no session): " .. agent:agent_key())
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

        -- Check if any agents already exist for this workspace — notify them
        if issue_or_branch then
            local meta = message.metadata or {}
            local existing = {}

            if meta.workspace then
                existing = Agent.find_by_workspace(meta.workspace)
            else
                local repo = message.repo or config.env("BOTSTER_REPO") or hub.detect_repo() or "unknown/repo"
                local issue_number, _ = parse_issue_or_branch(issue_or_branch)
                if issue_number then
                    for _, agent in ipairs(Agent.find_by_meta("issue_number", issue_number)) do
                        if agent.repo == repo then
                            existing[#existing + 1] = agent
                        end
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

    elseif msg_type == "create_accessory" then
        local session_name = message.session_name
        local workspace = message.workspace
        local profile = message.profile
        handle_create_accessory(workspace, session_name, profile, message.metadata)

    elseif msg_type == "delete_agent" or msg_type == "delete_session" then
        local session_id = message.id or message.agent_id or message.session_uuid or message.session_key
        if session_id then
            handle_delete_session(session_id, message.delete_worktree or false)
        else
            log.warn("command_message delete missing session identifier")
        end
    end
end)

-- ============================================================================
-- Async Worktree Creation Callbacks
-- ============================================================================

_event_subs[#_event_subs + 1] = events.on("worktree_created", function(info)
    log.info(string.format("Worktree created for %s at %s, resuming agent spawn",
        info.branch, info.path))

    local repo_root = worktree.repo_root()

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
    handle_create_accessory = handle_create_accessory,
    handle_delete_session = handle_delete_session,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
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
