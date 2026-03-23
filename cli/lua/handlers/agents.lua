-- Agent lifecycle handler (hot-reloadable)
--
-- Orchestrates agent and accessory creation/deletion with full lifecycle broadcasting.
--
-- Responsibilities:
-- - Parse issue-or-branch input into branch_name
-- - Find or create worktrees
-- - Resolve config via ConfigResolver (agents/accessories/workspaces)
-- - Spawn agents (single PTY, AI-driven) via Agent.new()
-- - Spawn accessories (single PTY, no AI autonomy) via Accessory.new()
-- - Auto-spawn workspace accessories when agent is created with a workspace
-- - Broadcast agent lifecycle events to connected clients
--
-- Both Agent and Accessory inherit from Session (lib/session.lua).
-- Session UUID is the primary key for everything.

local Agent = require("lib.agent")
local Accessory = require("lib.accessory")
local ConfigResolver = require("lib.config_resolver")
local TargetContext = require("lib.target_context")

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

local function resolve_target(target, metadata)
    return TargetContext.resolve({
        explicit = target,
        metadata = metadata,
        require_target_id = true,
        require_target_path = true,
    })
end

local function repo_label_for_target(target)
    return TargetContext.default_repo_label(target)
end

local function current_runtime_repo_root()
    return (worktree and worktree.repo_root and worktree.repo_root()) or nil
end

local function target_uses_current_runtime(target)
    local current_root = current_runtime_repo_root()
    return current_root ~= nil and target ~= nil and target.target_path == current_root
end

local function inspect_target(target)
    local registry = rawget(_G, "spawn_targets")
    if not registry or type(registry.inspect) ~= "function" or not target or not target.target_path then
        return nil
    end

    local ok, inspection = pcall(registry.inspect, target.target_path)
    if not ok then
        return nil
    end
    return inspection
end

local function matches_issue_for_target(agent, issue_number, target)
    if not issue_number then
        return false
    end
    return agent.metadata and agent.metadata.issue_number == issue_number and TargetContext.matches(agent, target)
end

-- ============================================================================
-- Agent Name Resolution
-- ============================================================================

--- Resolve agent name from user input.
-- @param device_root string|nil Path to ~/.botster
-- @param target_root string|nil Path to target root
-- @param agent_name string|nil Input from user/browser
-- @return string|nil Resolved agent name
-- @return string|nil Error message if resolution fails
local function resolve_agent_name(device_root, target_root, agent_name)
    -- Explicit agent name provided
    if agent_name and agent_name ~= "" then
        return agent_name, nil
    end

    -- Auto-select: list available agents
    local agents = ConfigResolver.list_agents(device_root, target_root)
    if #agents == 0 then
        return nil, "No agents found in config."
    elseif #agents == 1 then
        log.info(string.format("Auto-selected agent: %s", agents[1]))
        return agents[1], nil
    else
        return nil, string.format(
            "Multiple agents available (%s). Please specify an agent.",
            table.concat(agents, ", "))
    end
end

--- Pick the agent config from resolved config.
-- @param resolved table ConfigResolver.resolve_all() output
-- @param agent_name string Name of the agent to pick
-- @return table Single session config for Agent.new()
local function pick_agent_config(resolved, agent_name)
    local agent = resolved.agents[agent_name]
    if agent then
        return {
            name = agent_name,
            command = "bash",
            init_script = agent.initialization,
            notifications = true,
            forward_port = false,
        }
    end

    -- Fallback: pick the first agent
    for name, a in pairs(resolved.agents) do
        return {
            name = name,
            command = "bash",
            init_script = a.initialization,
            notifications = true,
            forward_port = false,
        }
    end

    return { name = "agent", command = "bash", notifications = true }
end

--- Pick an accessory config from resolved config.
-- @param resolved table ConfigResolver.resolve_all() output
-- @param accessory_name string Name of the accessory to pick
-- @return table|nil Single session config, or nil if not found
local function pick_accessory_config(resolved, accessory_name)
    local accessory = resolved.accessories[accessory_name]
    if accessory then
        return {
            name = accessory_name,
            command = "bash",
            init_script = accessory.initialization,
            notifications = false,
            forward_port = accessory.port_forward,
        }
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

-- Forward declaration so spawn_agent can call spawn_accessory
local spawn_accessory

--- Spawn an agent in an existing worktree.
--
-- @param branch_name string
-- @param wt_path string        Worktree filesystem path
-- @param prompt string          Task description
-- @param client table|nil       Requesting client (for dimensions)
-- @param agent_key string       Pre-computed agent key for status broadcasts
-- @param agent_name string      Agent name from config (e.g., "claude")
-- @param metadata table|nil     Plugin metadata
-- @param workspace_manifest table|nil  Workspace manifest { agents[], accessories[] }
-- @param target table           Explicit target context
-- @return Agent|nil             The created agent, or nil on error
-- @return string|nil            Error message (nil on success)
local function spawn_agent(branch_name, wt_path, prompt, client, agent_key, agent_name, metadata, workspace_manifest, target)
    local resolved_target, target_err = resolve_target(target, metadata)
    if not resolved_target then
        notify_lifecycle(agent_key, "failed", { error = tostring(target_err) })
        return nil, tostring(target_err)
    end

    local repo = resolved_target.target_repo or repo_label_for_target(resolved_target)
    local repo_root = resolved_target.target_path

    -- Broadcast: spawning PTYs
    notify_lifecycle(agent_key, "spawning_ptys")

    -- Resolve config across device + repo layers
    local device_root = config.data_dir and config.data_dir() or nil
    local resolved, err = ConfigResolver.resolve_all({
        device_root = device_root,
        repo_root = repo_root,
    })
    if not resolved then
        local msg = string.format("Config resolution failed for agent '%s': %s",
            tostring(agent_name), tostring(err))
        log.error(msg)
        notify_lifecycle(agent_key, "failed", { error = tostring(err) })
        return nil, msg
    end

    -- Pick the agent config
    local session_config = pick_agent_config(resolved, agent_name)

    -- Inject system prompt into worktree before init script runs.
    -- Uses a marker comment to prevent duplicate injection on re-spawn.
    local agent_config = resolved.agents[agent_name]
    if agent_config and agent_config.system_prompt and agent_config.system_prompt ~= "" then
        local claude_dir = wt_path .. "/.claude"
        local claude_md = claude_dir .. "/CLAUDE.md"
        local marker = "<!-- botster:system-prompt -->"
        local prompt_block = marker .. "\n" .. agent_config.system_prompt
        if not fs.exists(claude_dir) then
            fs.mkdir(claude_dir)
        end
        if fs.exists(claude_md) then
            local existing = fs.read(claude_md) or ""
            if not existing:find(marker, 1, true) then
                fs.write(claude_md, existing .. "\n\n" .. prompt_block)
                log.info(string.format("Appended system prompt to %s", claude_md))
            else
                log.debug(string.format("System prompt already present in %s, skipping", claude_md))
            end
        else
            fs.write(claude_md, prompt_block)
            log.info(string.format("Wrote system prompt to %s", claude_md))
        end
    end

    -- Default dimensions
    local dims = { rows = 24, cols = 80 }

    -- Workspace orchestration is explicit; no implicit branch-based grouping.
    local full_metadata = TargetContext.with_metadata(metadata, resolved_target)
    local workspace_name = full_metadata and full_metadata.workspace or nil
    local workspace_id = full_metadata and full_metadata.workspace_id or nil
    local workspace_metadata = full_metadata and full_metadata.workspace_metadata or nil
    local workspace_expect_new = full_metadata and full_metadata.workspace_expect_new or false
    local session_metadata = full_metadata
    if full_metadata and full_metadata.workspace_expect_new ~= nil then
        session_metadata = {}
        for k, v in pairs(full_metadata) do
            if k ~= "workspace_expect_new" then
                session_metadata[k] = v
            end
        end
    end

    local ok, agent = pcall(Agent.new, {
        repo = repo,
        branch_name = branch_name,
        worktree_path = wt_path,
        prompt = prompt,
        metadata = session_metadata,
        target_id = resolved_target.target_id,
        target_path = resolved_target.target_path,
        target_repo = resolved_target.target_repo,
        workspace = workspace_name,
        workspace_id = workspace_id,
        workspace_expect_new = workspace_expect_new,
        workspace_metadata = workspace_metadata,
        session_type = "agent",
        session = session_config,
        dims = dims,
        agent_key = agent_key,
        agent_name = agent_name,
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

    -- Auto-spawn accessories from workspace manifest
    if workspace_manifest and workspace_manifest.accessories then
        for _, acc_name in ipairs(workspace_manifest.accessories) do
            local acc_config = pick_accessory_config(resolved, acc_name)
            if acc_config then
                local acc_base_key = build_agent_key(repo, branch_name) .. "-" .. acc_name
                local acc_key = next_available_key(acc_base_key)
                local acc_metadata = {
                    workspace = workspace_name,
                    workspace_id = workspace_id,
                }
                acc_metadata = TargetContext.with_metadata(acc_metadata, resolved_target)
                spawn_accessory(
                    branch_name, wt_path, acc_name, acc_key, agent_name, acc_metadata, resolved, resolved_target
                )
            else
                log.warn(string.format("Workspace accessory '%s' not found in config, skipping", acc_name))
            end
        end
    end

    return agent
end

--- Spawn an accessory in an existing worktree.
--
-- @param branch_name string
-- @param wt_path string        Worktree filesystem path
-- @param accessory_name string Accessory name from config (e.g., "rails-server")
-- @param agent_key string       Pre-computed agent key
-- @param agent_name string      Agent name for config resolution
-- @param metadata table|nil     Plugin metadata
-- @param pre_resolved table|nil Already-resolved config (avoids re-resolving)
-- @param target table           Explicit target context
-- @return Accessory|nil
-- @return string|nil
spawn_accessory = function(branch_name, wt_path, accessory_name, agent_key, agent_name, metadata, pre_resolved, target)
    local resolved_target, target_err = resolve_target(target, metadata)
    if not resolved_target then
        return nil, tostring(target_err)
    end

    local repo = resolved_target.target_repo or repo_label_for_target(resolved_target)
    local repo_root = resolved_target.target_path

    local resolved = pre_resolved
    if not resolved then
        local device_root = config.data_dir and config.data_dir() or nil
        local err
        resolved, err = ConfigResolver.resolve_all({
            device_root = device_root,
            repo_root = repo_root,
        })
        if not resolved then
            log.error(string.format("Config resolution failed: %s", tostring(err)))
            return nil, tostring(err)
        end
    end

    local session_config = pick_accessory_config(resolved, accessory_name)
    if not session_config then
        -- Fall back to a raw shell with the given name
        session_config = { name = accessory_name, command = "bash" }
    end

    local full_metadata = TargetContext.with_metadata(metadata, resolved_target)
    local workspace_name = full_metadata and full_metadata.workspace or nil
    local workspace_id = full_metadata and full_metadata.workspace_id or nil
    local workspace_expect_new = full_metadata and full_metadata.workspace_expect_new or false
    local session_metadata = full_metadata
    if full_metadata and full_metadata.workspace_expect_new ~= nil then
        session_metadata = {}
        for k, v in pairs(full_metadata) do
            if k ~= "workspace_expect_new" then
                session_metadata[k] = v
            end
        end
    end

    local ok, agent = pcall(Accessory.new, {
        repo = repo,
        branch_name = branch_name,
        worktree_path = wt_path,
        session = session_config,
        metadata = session_metadata,
        target_id = resolved_target.target_id,
        target_path = resolved_target.target_path,
        target_repo = resolved_target.target_repo,
        workspace = workspace_name,
        workspace_id = workspace_id,
        workspace_expect_new = workspace_expect_new,
        dims = { rows = 24, cols = 80 },
        agent_key = agent_key,
        agent_name = agent_name,
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
-- @param agent_name string|nil       Agent name (e.g., "claude")
-- @param metadata table|nil          Plugin metadata
-- @param target table|nil            Explicit target context
-- @return Agent|nil
-- @return string|nil
local function handle_create_agent(issue_or_branch, prompt, from_worktree, client, agent_name, metadata, target)
    local early_id = issue_or_branch or "main"

    -- Interceptor: plugins can transform params or block creation
    local params = hooks.call("before_agent_create", {
        issue_or_branch = issue_or_branch,
        prompt = prompt,
        from_worktree = from_worktree,
        agent_name = agent_name,
        profile_name = agent_name,  -- backward compat for hook consumers
        metadata = metadata,
        target = target,
    })
    if params == nil then
        log.info("before_agent_create interceptor blocked agent creation")
        notify_lifecycle(early_id, "failed", { error = "Blocked by interceptor" })
        return nil, "Blocked by interceptor"
    end
    issue_or_branch = params.issue_or_branch
    prompt = params.prompt
    from_worktree = params.from_worktree
    agent_name = params.agent_name or params.profile_name  -- accept either from hooks
    metadata = params.metadata
    target = params.target

    local resolved_target, target_err = resolve_target(target, metadata)
    if not resolved_target then
        log.error(string.format("Target resolution failed: %s", tostring(target_err)))
        notify_lifecycle(early_id, "failed", { error = tostring(target_err) })
        return nil, tostring(target_err)
    end
    metadata = TargetContext.with_metadata(metadata, resolved_target)

    -- Resolve agent name
    local device_root = config.data_dir and config.data_dir() or nil
    local resolved_name, name_err = resolve_agent_name(device_root, resolved_target.target_path, agent_name)
    if name_err then
        log.error(string.format("Agent resolution failed: %s", name_err))
        notify_lifecycle(early_id, "failed", { error = name_err })
        return nil, "Agent resolution failed: " .. name_err
    end
    agent_name = resolved_name

    -- Check for workspace manifest to auto-spawn accessories
    local workspace_manifest = nil
    if metadata and metadata.workspace_config then
        workspace_manifest = metadata.workspace_config
    end

    -- Main repo mode: no issue_or_branch AND no from_worktree
    if not issue_or_branch and not from_worktree then
        local repo = resolved_target.target_repo or repo_label_for_target(resolved_target)
        local base_key = build_agent_key(repo, "main")
        local suffix = Agent.next_instance_suffix(base_key)
        local agent_key = base_key .. (suffix or "")
        return spawn_agent(
            "main", resolved_target.target_path, prompt, client, agent_key, agent_name, metadata, workspace_manifest,
            resolved_target
        )
    end

    local _, branch_name = parse_issue_or_branch(issue_or_branch)

    if prompt == "" then
        prompt = nil
    end

    local repo = resolved_target.target_repo or repo_label_for_target(resolved_target)
    local agent_key = build_agent_key(repo, branch_name)
    agent_key = next_available_key(agent_key)

    local target_inspection = inspect_target(resolved_target)
    local worktree_root = (target_inspection and target_inspection.repo_root) or resolved_target.target_path

    -- Non-git mode
    if not (target_inspection and target_inspection.is_git_repo) then
        log.info(string.format("No git repo — spawning %s directly in %s", branch_name, resolved_target.target_path))
        return spawn_agent(
            branch_name, resolved_target.target_path, prompt, client, agent_key, agent_name, metadata,
            workspace_manifest, resolved_target
        )
    end

    -- Find or create worktree
    local wt_path = from_worktree
    if not wt_path then
        if target_uses_current_runtime(resolved_target) then
            wt_path = worktree.find(branch_name)
        else
            wt_path = worktree.find_for_root(worktree_root, branch_name)
        end
    end

    if not wt_path then
        local head_path = worktree_root .. "/.git/HEAD"
        local f = io.open(head_path, "r")
        if f then
            local head = f:read("*l")
            f:close()
            local main_branch = head and head:match("^ref: refs/heads/(.+)$")
            if main_branch == branch_name then
                wt_path = resolved_target.target_path
                log.info(string.format(
                    "Branch '%s' is main repo checkout, using %s", branch_name, resolved_target.target_path
                ))
            end
        end
    end

    if not wt_path then
        notify_lifecycle(agent_key, "creating_worktree")
        log.info(string.format("No worktree found for %s, queueing async creation...", branch_name))

        -- Stash workspace_manifest and agent_name in metadata so they survive
        -- the Rust round-trip (WorktreeCreateResult only carries metadata, not these fields)
        -- Use plain Lua table copy here so this works in headless runtimes (no global `vim`).
        local async_metadata = TargetContext.with_metadata(metadata, resolved_target)
        async_metadata._workspace_manifest = workspace_manifest
        async_metadata._agent_name = agent_name

        if target_uses_current_runtime(resolved_target) then
            worktree.create_async({
                agent_key = agent_key,
                branch = branch_name,
                prompt = prompt,
                metadata = async_metadata,
                profile_name = agent_name,  -- Rust reads profile_name from this table
                client_rows = 24,
                client_cols = 80,
            })
            return nil  -- Agent spawning continues in worktree_created event handler
        end

        local ok, created_or_err = pcall(worktree.create_for_root, worktree_root, branch_name)
        if not ok then
            notify_lifecycle(agent_key, "failed", { error = tostring(created_or_err) })
            return nil, tostring(created_or_err)
        end
        wt_path = created_or_err
    else
        log.info(string.format("Worktree found for %s at %s", branch_name, wt_path))
    end

    return spawn_agent(
        branch_name, wt_path, prompt, client, agent_key, agent_name, metadata, workspace_manifest, resolved_target
    )
end

--- Handle a request to create an accessory.
-- @param workspace_id string|nil       Workspace identifier
-- @param workspace_name string|nil     Workspace display name
-- @param accessory_name string         Accessory name from config (e.g., "rails-server")
-- @param agent_name string|nil         Agent name for config resolution
-- @param metadata table|nil            Plugin metadata
-- @param target table|nil              Explicit target context
-- @return Accessory|nil
-- @return string|nil
local function handle_create_accessory(workspace_id, workspace_name, accessory_name, agent_name, metadata, target)
    if not accessory_name then
        return nil, "accessory_name is required for accessories"
    end

    local resolved_target, target_err = resolve_target(target, metadata)
    if not resolved_target and not workspace_id and not workspace_name then
        return nil, tostring(target_err)
    end

    -- Find worktree from workspace or use repo root
    local wt_path = resolved_target and resolved_target.target_path or nil
    local branch_name = "main"

    -- If workspace provided, prefer explicit ID lookup.
    if workspace_id then
        for _, existing in ipairs(Agent.list()) do
            if existing._workspace_id == workspace_id then
                wt_path = existing.worktree_path
                branch_name = existing.branch_name
                workspace_name = workspace_name or existing._workspace_name
                resolved_target = TargetContext.from_session(existing)
                break
            end
        end
    elseif workspace_name then
        local existing = Agent.find_by_workspace(workspace_name, resolved_target)
        if #existing > 0 then
            wt_path = existing[1].worktree_path
            branch_name = existing[1].branch_name
            workspace_id = existing[1]._workspace_id
            resolved_target = TargetContext.from_session(existing[1])
        end
    end

    if not resolved_target then
        return nil, tostring(target_err or "target_id is required")
    end
    if not wt_path then
        return nil, "target_path is required"
    end

    local repo = resolved_target.target_repo or repo_label_for_target(resolved_target)
    local base_key = build_agent_key(repo, branch_name) .. "-" .. accessory_name
    local agent_key = next_available_key(base_key)

    metadata = TargetContext.with_metadata(metadata, resolved_target)
    metadata.workspace = workspace_name
    metadata.workspace_id = workspace_id

    return spawn_accessory(
        branch_name, wt_path, accessory_name, agent_key, agent_name, metadata, nil, resolved_target
    )
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
        local command_target = {
            target_id = message.target_id,
            target_path = message.target_path,
            target_repo = message.target_repo,
        }

        -- Check if any agents already exist for this workspace — notify them
        if issue_or_branch then
            local meta = TargetContext.with_metadata(message.metadata, command_target)
            if message.workspace_id and not meta.workspace_id then
                meta.workspace_id = message.workspace_id
            end
            if message.workspace_name and not meta.workspace then
                meta.workspace = message.workspace_name
            end
            local resolved_target = TargetContext.resolve({
                explicit = command_target,
                metadata = meta,
            })
            local existing = {}

            if meta.workspace_id then
                for _, session in ipairs(Agent.list()) do
                    if session._workspace_id == meta.workspace_id then
                        existing[#existing + 1] = session
                    end
                end
            elseif meta.workspace then
                existing = Agent.find_by_workspace(meta.workspace, resolved_target)
            else
                local issue_number, _ = parse_issue_or_branch(issue_or_branch)
                if issue_number then
                    for _, agent in ipairs(Agent.find_by_meta("issue_number", issue_number)) do
                        if matches_issue_for_target(agent, issue_number, resolved_target) then
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
            local meta = TargetContext.with_metadata(message.metadata, command_target)
            if message.workspace_id and not meta.workspace_id then
                meta.workspace_id = message.workspace_id
            end
            if message.workspace_name and not meta.workspace then
                meta.workspace = message.workspace_name
            end
            if message.invocation_url and not meta.invocation_url then
                meta.invocation_url = message.invocation_url
            end
            local issue_number, _ = parse_issue_or_branch(issue_or_branch)
            if issue_number and not meta.issue_number then
                meta.issue_number = issue_number
            end
            -- Accept both "profile" (legacy) and "agent_name" (new)
            local agent_name = message.agent_name or message.profile
            handle_create_agent(issue_or_branch, message.prompt, message.from_worktree, nil, agent_name, meta, command_target)
        else
            log.warn("command_message create_agent missing issue_or_branch")
        end

    elseif msg_type == "create_accessory" then
        local accessory_name = message.accessory_name or message.session_name or message.name
        local workspace_id = message.workspace_id
        local workspace_name = message.workspace_name
        local agent_name = message.agent_name or message.profile
        handle_create_accessory(workspace_id, workspace_name, accessory_name, agent_name, message.metadata, {
            target_id = message.target_id,
            target_path = message.target_path,
            target_repo = message.target_repo,
        })

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

    local target = TargetContext.resolve({
        metadata = info.metadata or {},
    })

    hooks.notify("worktree_created", {
        path = info.path,
        branch = info.branch,
        repo = target and target.target_repo or nil,
        agent_key = info.agent_key,
        metadata = info.metadata or {},
    })

    local client = { rows = info.client_rows, cols = info.client_cols }

    -- Extract stashed fields from metadata (Rust doesn't carry these directly)
    local metadata = info.metadata or {}
    local workspace_manifest = metadata._workspace_manifest
    local agent_name = metadata._agent_name or info.profile_name
    metadata._workspace_manifest = nil
    metadata._agent_name = nil

    spawn_agent(
        info.branch,
        info.path,
        info.prompt,
        client,
        info.agent_key,
        agent_name,
        metadata,
        workspace_manifest,
        target
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
