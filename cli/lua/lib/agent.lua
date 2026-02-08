-- Agent class for managing PTY sessions in a worktree.
--
-- Each Agent instance tracks:
-- - Repository and issue/branch metadata
-- - One or more named PTY sessions (cli, server, etc.)
-- - Worktree path and lifecycle state
-- - Environment variables for spawned processes
--
-- Manages agent lifecycle: creation, tracking, metadata, and cleanup.
--
-- This module is hot-reloadable; state is persisted via core.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("core.state")
local hooks = require("core.hooks")

local Agent = state.class("Agent")

-- Agent registry (persistent across reloads)
local agents = state.get("agent_registry", {})

-- Sequential port counter for forward_port sessions (persistent across reloads)
local port_state = state.get("agent_port_state", { next_port = 8080 })

-- =============================================================================
-- Default Session Configurations
-- =============================================================================

--- Default session config: CLI only.
-- @return Table of session configs keyed by name
function Agent.default_sessions()
    return {
        cli = {
            command = "bash",
            init_script = ".botster_init",
            notifications = true,
            forward_port = false,
        },
    }
end

--- Default session config: CLI + server.
-- @return Table of session configs keyed by name
function Agent.default_sessions_with_server()
    return {
        cli = {
            command = "bash",
            init_script = ".botster_init",
            notifications = true,
            forward_port = false,
        },
        server = {
            command = "bash",
            init_script = ".botster_server",
            notifications = false,
            forward_port = true,
        },
    }
end

-- =============================================================================
-- Constructor
-- =============================================================================

--- Create a new Agent and spawn its PTY sessions.
--
-- Config table:
--   repo            string   (required)  "owner/repo"
--   issue_number    number   (optional)
--   branch_name     string   (required)
--   worktree_path   string   (required)
--   prompt          string   (optional)  task description
--   invocation_url  string   (optional)  GitHub URL
--   sessions        table    (optional)  named session configs, defaults to default_sessions()
--   env             table    (optional)  base environment variables
--   dims            table    (optional)  { rows = 24, cols = 80 }
--
-- @param config Table of agent configuration
-- @return Agent instance
function Agent.new(config)
    assert(config.repo, "Agent.new requires config.repo")
    assert(config.branch_name, "Agent.new requires config.branch_name")
    assert(config.worktree_path, "Agent.new requires config.worktree_path")

    -- NOTE: before_agent_create hook fires in handlers/agents.lua (high-level params).
    -- Agent.new() is a low-level constructor and does NOT re-fire the hook.

    local self = setmetatable({
        repo = config.repo,
        issue_number = config.issue_number,
        branch_name = config.branch_name,
        worktree_path = config.worktree_path,
        prompt = config.prompt,
        invocation_url = config.invocation_url,
        created_at = os.time(),
        status = "running",
        sessions = {},  -- name -> PtySessionHandle
    }, Agent)

    local key = self:agent_key()

    -- Write prompt file to worktree if prompt provided
    if config.prompt then
        local prompt_path = config.worktree_path .. "/.botster_prompt"
        local ok, err = pcall(fs.write, prompt_path, config.prompt)
        if not ok then
            log.warn(string.format("Failed to write prompt file: %s", tostring(err)))
        end
    end

    -- Build environment variables
    local env = self:build_env(config.env)

    -- Determine dimensions
    local rows = 24
    local cols = 80
    if config.dims then
        rows = config.dims.rows or 24
        cols = config.dims.cols or 80
    end

    -- Determine session configuration
    local session_configs = config.sessions or Agent.default_sessions()

    -- Spawn each configured session
    for name, session_config in pairs(session_configs) do
        local spawn_config = {
            worktree_path = config.worktree_path,
            command = session_config.command or "bash",
            env = env,
            detect_notifications = session_config.notifications or false,
            rows = rows,
            cols = cols,
        }

        -- Build init_commands from init_script
        if session_config.init_script then
            local script_path = config.worktree_path .. "/" .. session_config.init_script
            if fs.exists(script_path) then
                spawn_config.init_commands = { "source " .. session_config.init_script }
            else
                log.debug(string.format("Init script not found: %s", script_path))
            end
        end

        -- Allocate port for forward_port sessions
        if session_config.forward_port then
            local port = port_state.next_port
            port_state.next_port = port + 1
            spawn_config.port = port
            -- Also inject port into env for the spawned process
            spawn_config.env = spawn_config.env or {}
            spawn_config.env.BOTSTER_TUNNEL_PORT = tostring(port)
        end

        local ok, handle = pcall(pty.spawn, spawn_config)
        if ok then
            self.sessions[name] = handle
            log.info(string.format("Agent %s: spawned session '%s'", key, name))
        else
            log.error(string.format("Agent %s: failed to spawn session '%s': %s",
                key, name, tostring(handle)))
        end
    end

    -- Register PTY handles with HandleCache for Rust-side access
    -- (enables write_pty, resize_pty, forwarders, etc.)
    local session_count = self:session_count()
    log.info(string.format("Agent %s: spawned %d sessions, preparing to register", key, session_count))

    -- Log what sessions we have
    for name, handle in pairs(self.sessions) do
        log.info(string.format("Agent %s: session '%s' = %s", key, name, tostring(handle)))
    end

    if session_count > 0 then
        local ok, result = pcall(hub.register_agent, key, self.sessions)
        if ok then
            self.agent_index = result
            log.info(string.format("Agent %s: registered with HandleCache at index %d", key, result))
        else
            log.error(string.format("Agent %s: failed to register with HandleCache: %s", key, tostring(result)))
        end
    else
        log.warn(string.format("Agent %s: no sessions to register (all PTY spawns may have failed)", key))
    end

    -- Register in agent registry
    agents[key] = self

    -- Notify observers
    hooks.notify("after_agent_create", self)

    log.info(string.format("Agent created: %s (sessions: %d)", key, self:session_count()))
    return self
end

-- =============================================================================
-- Instance Methods
-- =============================================================================

--- Generate the agent key.
-- Format: repo-name-issue_number (slashes replaced with dashes)
-- @return string agent key
function Agent:agent_key()
    local repo_safe = self.repo:gsub("/", "-")
    if self.issue_number then
        return repo_safe .. "-" .. tostring(self.issue_number)
    else
        local branch_safe = self.branch_name:gsub("/", "-")
        return repo_safe .. "-" .. branch_safe
    end
end

--- Close the agent and clean up resources.
-- @param delete_worktree boolean Whether to queue worktree deletion
function Agent:close(delete_worktree)
    local key = self:agent_key()

    -- Notify observers
    hooks.notify("before_agent_close", self)

    -- Unregister from HandleCache (before killing sessions)
    local ok, err = pcall(hub.unregister_agent, key)
    if not ok then
        log.warn(string.format("Agent %s: failed to unregister from HandleCache: %s", key, tostring(err)))
    end

    -- Kill all sessions
    for name, handle in pairs(self.sessions) do
        local ok2, err2 = pcall(function() handle:kill() end)
        if not ok2 then
            log.warn(string.format("Agent %s: error killing session '%s': %s",
                key, name, tostring(err2)))
        end
    end
    self.sessions = {}
    self.status = "closed"

    -- Remove from registry
    agents[key] = nil

    -- Queue worktree deletion if requested
    if delete_worktree then
        local ok3, err3 = pcall(worktree.delete, self.worktree_path, self.branch_name)
        if not ok3 then
            log.warn(string.format("Agent %s: failed to delete worktree: %s",
                key, tostring(err3)))
        end
    end

    -- Notify observers
    hooks.notify("after_agent_close", self)

    log.info(string.format("Agent closed: %s (delete_worktree=%s)", key, tostring(delete_worktree or false)))
end

--- Count active sessions.
-- @return number
function Agent:session_count()
    local count = 0
    for _ in pairs(self.sessions) do
        count = count + 1
    end
    return count
end

--- Build environment variables for spawned sessions.
-- @param base_env table Optional base env vars to merge
-- @return table Environment variables
function Agent:build_env(base_env)
    local env = {}
    -- Copy base env first
    if base_env then
        for k, v in pairs(base_env) do
            env[k] = v
        end
    end
    env.BOTSTER_REPO = self.repo
    if self.issue_number then
        env.BOTSTER_ISSUE_NUMBER = tostring(self.issue_number)
    end
    env.BOTSTER_BRANCH_NAME = self.branch_name
    env.BOTSTER_WORKTREE_PATH = self.worktree_path
    if self.prompt then
        env.BOTSTER_TASK_DESCRIPTION = self.prompt
    end
    -- Fire filter hook for customization
    env = hooks.call("filter_agent_env", env, self) or env
    return env
end

--- Get agent metadata for clients.
-- Returns a serializable table of agent info.
-- @return table Agent info
function Agent:info()
    local key = self:agent_key()

    -- Determine server state
    local has_server_pty = self.sessions.server ~= nil
    local server_running = false
    local port = nil

    if has_server_pty then
        local server = self.sessions.server
        local ok, p = pcall(function() return server:port() end)
        if ok and p then
            port = p
        end
        local ok2, alive = pcall(function() return server:is_alive() end)
        if ok2 then
            server_running = alive
        end
    end

    return {
        id = key,
        repo = self.repo,
        issue_number = self.issue_number,
        branch_name = self.branch_name,
        worktree_path = self.worktree_path,
        status = self.status,
        has_server_pty = has_server_pty,
        server_running = server_running,
        port = port,
        created_at = self.created_at,
    }
end

-- =============================================================================
-- Module-Level Functions (on the Agent class table)
-- =============================================================================

--- Get an agent by key.
-- @param key string Agent key
-- @return Agent or nil
function Agent.get(key)
    return agents[key]
end

--- Get an agent by its HandleCache index.
-- Unlike list-based lookup, this is stable across agent deletions because
-- it matches against the index assigned at registration time.
-- @param index number HandleCache index (0-based)
-- @return Agent or nil
function Agent.get_by_index(index)
    for _, agent in pairs(agents) do
        if agent.agent_index == index then
            return agent
        end
    end
    return nil
end

--- List all agents in creation order.
-- @return array of Agent instances
function Agent.list()
    local result = {}
    for _, agent in pairs(agents) do
        table.insert(result, agent)
    end
    -- Sort by creation time for stable ordering
    table.sort(result, function(a, b)
        return (a.created_at or 0) < (b.created_at or 0)
    end)
    return result
end

--- Count active agents.
-- @return number
function Agent.count()
    local count = 0
    for _ in pairs(agents) do
        count = count + 1
    end
    return count
end

--- Get info tables for all agents (for client broadcast).
-- @return array of info tables
function Agent.all_info()
    local result = {}
    for _, agent in ipairs(Agent.list()) do
        table.insert(result, agent:info())
    end
    return result
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================

function Agent._before_reload()
    log.info("agent.lua reloading (persistent metatable -- instances auto-upgrade)")
end

function Agent._after_reload()
    log.info(string.format("agent.lua reloaded -- %d agents preserved", Agent.count()))
end

return Agent
