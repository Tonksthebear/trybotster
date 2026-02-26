-- Agent class for managing PTY sessions in a worktree.
--
-- Each Agent instance tracks:
-- - Repository and issue/branch metadata
-- - One or more named PTY sessions in deterministic order
-- - Worktree path and lifecycle state
-- - Environment variables for spawned processes
--
-- Sessions are ordered: agent always first (index 0 in Rust), then
-- alphabetical. The order is set at creation time via config resolver.
--
-- Manages agent lifecycle: creation, tracking, metadata, and cleanup.
--
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local hooks = require("hub.hooks")

local Agent = state.class("Agent")

-- Agent registry (persistent across reloads)
local agents = state.get("agent_registry", {})

-- Sequential port counter for forward_port sessions (persistent across reloads)
local port_state = state.get("agent_port_state", { next_port = 8080 })

-- =============================================================================
-- Constructor
-- =============================================================================

--- Create a new Agent and spawn its PTY sessions.
--
-- Config table:
--   repo            string   (required)  "owner/repo"
--   branch_name     string   (required)
--   worktree_path   string   (required)
--   prompt          string   (optional)  task description
--   metadata        table    (optional)  plugin key-value store (e.g., issue_number, invocation_url)
--   sessions        array    (required)  ordered session configs from config resolver:
--                              { name, command, init_script, notifications, forward_port }
--   env             table    (optional)  base environment variables
--   dims            table    (optional)  { rows = 24, cols = 80 }
--
-- @param config Table of agent configuration
-- @return Agent instance
function Agent.new(config)
    assert(config.repo, "Agent.new requires config.repo")
    assert(config.branch_name, "Agent.new requires config.branch_name")
    assert(config.worktree_path, "Agent.new requires config.worktree_path")
    assert(config.sessions and #config.sessions > 0, "Agent.new requires config.sessions array")

    -- NOTE: before_agent_create hook fires in handlers/agents.lua (high-level params).
    -- Agent.new() is a low-level constructor and does NOT re-fire the hook.

    -- Build metadata table: explicit metadata + backward compat for legacy fields
    local metadata = {}
    if config.metadata then
        for k, v in pairs(config.metadata) do
            metadata[k] = v
        end
    end
    -- Backward compat: accept legacy top-level fields into metadata
    if config.issue_number and not metadata.issue_number then
        metadata.issue_number = config.issue_number
    end
    if config.invocation_url and not metadata.invocation_url then
        metadata.invocation_url = config.invocation_url
    end

    local self = setmetatable({
        _agent_key = config.agent_key,  -- explicit key (may include suffix for multi-agent)
        repo = config.repo,
        branch_name = config.branch_name,
        worktree_path = config.worktree_path,
        prompt = config.prompt,
        metadata = metadata,
        profile_name = config.profile_name,
        created_at = os.time(),
        status = "running",
        title = nil,          -- window title from OSC 0/2 (set by pty_title_changed hook)
        cwd = nil,            -- current working directory from OSC 7 (set by pty_cwd_changed hook)
        notification = false, -- true when OSC notification fired, cleared by client
        sessions = {},        -- name -> PtySessionHandle (for lookup by name)
        session_order = {},   -- ordered array of { name, port_forward, port }
        _session_configs = config.sessions,  -- original session configs from creation (for available_session_types)
        _inbox = {},          -- inter-agent message inbox: array of envelope tables
    }, Agent)

    local key = self:agent_key()

    -- Write context.json to worktree for init scripts and post-mortem inspection.
    -- Skip on main checkout (where .git is a directory) to avoid polluting the
    -- repo's .botster/ config directory. In worktrees, .git is a file.
    local git_path = config.worktree_path .. "/.git"
    self._has_context_file = fs.exists(git_path) and not fs.is_dir(git_path)
    if self._has_context_file then
        self:_sync_context_json()
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

    -- Ordered array of PtySessionHandle for hub.register_agent()
    local ordered_handles = {}

    -- Spawn sessions in order (ipairs guarantees deterministic iteration)
    for _, session_config in ipairs(config.sessions) do
        local name = session_config.name

        -- Shallow-copy env per session to prevent PORT leaking across sessions
        local session_env = {}
        for k, v in pairs(env) do
            session_env[k] = v
        end

        local spawn_config = {
            worktree_path = config.worktree_path,
            command = session_config.command or "bash",
            env = session_env,
            detect_notifications = session_config.notifications or false,
            agent_key = key,
            session_name = name,
            rows = rows,
            cols = cols,
        }

        -- Build init_commands from init_script (absolute path from config resolver)
        if session_config.init_script then
            if fs.exists(session_config.init_script) then
                spawn_config.init_commands = { "source " .. session_config.init_script }
            else
                log.debug(string.format("Init script not found: %s", session_config.init_script))
            end
        end

        -- Allocate port for forward_port sessions
        local port = nil
        if session_config.forward_port then
            port = port_state.next_port
            port_state.next_port = port + 1
            spawn_config.port = port
            session_env.PORT = tostring(port)
        end

        local ok, handle = pcall(pty.spawn, spawn_config)
        if ok then
            self.sessions[name] = handle
            ordered_handles[#ordered_handles + 1] = handle
            self.session_order[#self.session_order + 1] = {
                name = name,
                port_forward = session_config.forward_port or false,
                port = port,
            }
            log.info(string.format("Agent %s: spawned session '%s' (pty_index %d)", key, name, #ordered_handles - 1))
        else
            log.error(string.format("Agent %s: failed to spawn session '%s': %s",
                key, name, tostring(handle)))
        end
    end

    -- Register PTY handles with HandleCache for Rust-side access
    -- (enables write_pty, resize_pty, forwarders, etc.)
    local session_count = #ordered_handles
    log.info(string.format("Agent %s: spawned %d sessions, preparing to register", key, session_count))

    if session_count > 0 then
        local ok, result = pcall(hub.register_agent, key, ordered_handles)
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

    log.info(string.format("Agent created: %s (sessions: %d)", key, session_count))
    return self
end

-- =============================================================================
-- Instance Methods
-- =============================================================================

--- Generate the agent key.
-- Format: repo-name-branch_name[-N] (slashes replaced with dashes)
-- @return string agent key
function Agent:agent_key()
    if self._agent_key then
        return self._agent_key
    end
    -- Fallback: derive from repo + branch_name (only if _agent_key not set)
    local repo_safe = self.repo:gsub("/", "-")
    local branch_safe = self.branch_name:gsub("/", "-")
    return repo_safe .. "-" .. branch_safe
end

--- Set a metadata value and sync context.json if applicable.
-- @param key string Metadata key
-- @param value any Metadata value
function Agent:set_meta(key, value)
    self.metadata[key] = value
    self:_sync_context_json()
end

--- Get a metadata value.
-- @param key string Metadata key
-- @return any Metadata value or nil
function Agent:get_meta(key)
    return self.metadata[key]
end

--- Sync context.json with current agent state.
-- Only writes if the agent has a context file (i.e., running in a worktree).
function Agent:_sync_context_json()
    if not self._has_context_file then return end
    local context = {
        repo = self.repo,
        branch_name = self.branch_name,
        prompt = self.prompt,
        metadata = self.metadata,
        created_at = os.date("!%Y-%m-%dT%H:%M:%SZ", self.created_at),
    }
    local context_dir = self.worktree_path .. "/.botster"
    if not fs.exists(context_dir) then
        fs.mkdir(context_dir)
    end
    local ok, err = pcall(fs.write, context_dir .. "/context.json", json.encode(context))
    if not ok then
        log.warn(string.format("Failed to sync context.json: %s", tostring(err)))
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
    self.session_order = {}
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
    return #(self.session_order or {})
end

--- Add a new PTY session to a running agent.
--
-- Spawns a new PTY in the agent's worktree and re-registers all handles
-- with HandleCache so clients see the new session immediately.
--
-- Config table:
--   name             string   (required)  session name (e.g., "shell", "server")
--   command          string   (optional)  command to run (default "bash")
--   init_script      string   (optional)  absolute path to init script
--   notifications    boolean  (optional)  enable OSC notification detection
--   forward_port     boolean  (optional)  allocate a PORT for this session
--
-- @param session_config table Session configuration
-- @return number|nil New pty_index, or nil on error
function Agent:add_session(session_config)
    assert(session_config.name, "add_session requires config.name")

    local key = self:agent_key()
    local name = session_config.name

    -- Deduplicate session names: shell, shell-2, shell-3, ...
    if self.sessions[name] then
        local i = 2
        while self.sessions[name .. "-" .. i] do
            i = i + 1
        end
        name = name .. "-" .. i
    end

    -- Build environment
    local env = self:build_env()

    -- Allocate port if requested
    local port = nil
    if session_config.forward_port then
        port = port_state.next_port
        port_state.next_port = port + 1
        env.PORT = tostring(port)
    end

    local spawn_config = {
        worktree_path = self.worktree_path,
        command = session_config.command or "bash",
        env = env,
        detect_notifications = session_config.notifications or false,
        agent_key = key,
        session_name = name,
        rows = 24,
        cols = 80,
    }

    if session_config.init_script then
        if fs.exists(session_config.init_script) then
            spawn_config.init_commands = { "source " .. session_config.init_script }
        else
            log.warn(string.format("Init script not found: %s", session_config.init_script))
        end
    end

    if port then
        spawn_config.port = port
    end

    local ok, handle = pcall(pty.spawn, spawn_config)
    if not ok then
        log.error(string.format("Agent %s: failed to spawn session '%s': %s",
            key, name, tostring(handle)))
        return nil
    end

    -- Add to session tracking
    self.sessions[name] = handle
    self.session_order[#self.session_order + 1] = {
        name = name,
        port_forward = session_config.forward_port or false,
        port = port,
    }

    local new_pty_index = #self.session_order - 1  -- 0-based
    log.info(string.format("Agent %s: spawned session '%s' (pty_index %d)", key, name, new_pty_index))

    -- Re-register all PTY handles with HandleCache (replace semantics)
    local ordered_handles = {}
    for _, entry in ipairs(self.session_order) do
        local session_handle = self.sessions[entry.name]
        if session_handle then
            ordered_handles[#ordered_handles + 1] = session_handle
        end
    end

    local reg_ok, result = pcall(hub.register_agent, key, ordered_handles)
    if reg_ok then
        self.agent_index = result
        log.info(string.format("Agent %s: re-registered with HandleCache at index %d (%d PTYs)",
            key, result, #ordered_handles))
    else
        log.error(string.format("Agent %s: failed to re-register: %s", key, tostring(result)))
    end

    -- Notify observers so clients get updated session list
    hooks.notify("agent_session_added", {
        agent = self:info(),
        session_name = name,
        pty_index = new_pty_index,
    })

    return new_pty_index
end

--- List available session types for adding to this agent.
-- Returns the agent's configured session types (from creation) plus a raw "shell" option.
-- Uses stored _session_configs rather than re-resolving from disk, so the types
-- always match what was available when the agent was created.
-- @return array of { name, label, description, raw, initialization, port_forward }
function Agent:available_session_types()
    local types = {}

    -- Always offer raw shell first
    types[#types + 1] = {
        name = "shell",
        label = "Shell",
        description = "Raw bash shell",
        raw = true,
    }

    -- Add configured session types from the agent's creation config
    if self._session_configs then
        for _, session in ipairs(self._session_configs) do
            -- Skip "agent" — that's the main session, not something you'd add
            if session.name ~= "agent" then
                types[#types + 1] = {
                    name = session.name,
                    label = session.name:sub(1, 1):upper() .. session.name:sub(2),
                    description = session.forward_port and "With port forwarding" or "From profile config",
                    initialization = session.init_script,
                    port_forward = session.forward_port,
                    raw = false,
                }
            end
        end
    end

    return types
end

--- Remove a PTY session from a running agent.
--
-- Kills the PTY process, removes it from tracking, and re-registers the
-- remaining handles with HandleCache. Cannot remove session at index 0
-- (the primary agent session).
--
-- @param pty_index number 0-based PTY index to remove
-- @return boolean true on success, false on error
function Agent:remove_session(pty_index)
    -- Never remove the primary session (index 0)
    if pty_index < 1 then
        log.warn("Cannot remove primary session (index 0)")
        return false
    end

    -- session_order is 1-based Lua array, pty_index is 0-based
    local order_index = pty_index + 1
    local entry = self.session_order[order_index]
    if not entry then
        log.warn(string.format("remove_session: invalid pty_index %d", pty_index))
        return false
    end

    local key = self:agent_key()
    local name = entry.name

    -- Kill the PTY process
    local handle = self.sessions[name]
    if handle then
        local ok, err = pcall(function() handle:kill() end)
        if not ok then
            log.warn(string.format("Agent %s: error killing session '%s': %s", key, name, tostring(err)))
        end
    end

    -- Remove from tracking
    self.sessions[name] = nil
    table.remove(self.session_order, order_index)

    log.info(string.format("Agent %s: removed session '%s' (was pty_index %d)", key, name, pty_index))

    -- Re-register remaining PTY handles with HandleCache
    local ordered_handles = {}
    for _, e in ipairs(self.session_order) do
        local session_handle = self.sessions[e.name]
        if session_handle then
            ordered_handles[#ordered_handles + 1] = session_handle
        end
    end

    if #ordered_handles > 0 then
        local reg_ok, result = pcall(hub.register_agent, key, ordered_handles)
        if reg_ok then
            self.agent_index = result
            log.info(string.format("Agent %s: re-registered with HandleCache at index %d (%d PTYs)",
                key, result, #ordered_handles))
        else
            log.error(string.format("Agent %s: failed to re-register: %s", key, tostring(result)))
        end
    end

    -- Notify observers
    hooks.notify("agent_session_removed", {
        agent = self:info(),
        session_name = name,
        pty_index = pty_index,
    })

    return true
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
    -- Inherit TERM from the daemon's environment so the inner PTY
    -- advertises the correct terminal capabilities (kitty keyboard, etc.).
    -- Agent config can override via base_env; fall back to xterm-256color
    -- for headless environments (systemd, cron) where TERM may be unset.
    env.TERM = env.TERM or os.getenv("TERM") or "xterm-256color"
    env.BOTSTER_WORKTREE_PATH = self.worktree_path
    env.BOTSTER_AGENT_KEY = self:agent_key()
    env.BOTSTER_HUB_ID = hub.server_id() or ""
    -- Fire filter hook for customization
    env = hooks.call("filter_agent_env", env, self) or env
    return env
end

--- Get agent metadata for clients.
-- Returns a serializable table of agent info.
-- Includes both new sessions[] array and backward-compat fields.
-- @return table Agent info
function Agent:info()
    local key = self:agent_key()

    -- Build sessions array from session_order
    local sessions_info = {}
    for _, entry in ipairs(self.session_order or {}) do
        local session_info = {
            name = entry.name,
            port_forward = entry.port_forward,
        }
        -- Get port from the PTY handle if port_forward is set
        if entry.port then
            session_info.port = entry.port
        end
        sessions_info[#sessions_info + 1] = session_info
    end

    -- Backward-compat: derive has_server_pty/port from sessions
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

    -- Build display name: prefer OSC title, fall back to branch_name + suffix
    local display_name
    if self.title and self.title ~= "" then
        display_name = self.title
    else
        display_name = self.branch_name
        local base_key = (function()
            local repo_safe = self.repo:gsub("/", "-")
            return repo_safe .. "-" .. self.branch_name:gsub("/", "-")
        end)()
        if #key > #base_key and key:sub(1, #base_key) == base_key then
            display_name = self.branch_name .. key:sub(#base_key + 1)
        end
    end

    return {
        id = key,
        display_name = display_name,
        title = self.title,
        cwd = self.cwd,
        profile_name = self.profile_name,
        repo = self.repo,
        metadata = self.metadata,
        branch_name = self.branch_name,
        worktree_path = self.worktree_path,
        in_worktree = self._has_context_file,
        status = self.status,
        -- New: ordered sessions array
        sessions = sessions_info,
        notification = self.notification or false,
        -- Backward compat (browser checks sessions first, falls back to these)
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

--- Find all agents matching a base key (ignoring instance suffix).
-- Returns agents whose key equals base_key or starts with base_key followed by "-".
-- @param base_key string The base agent key (without instance suffix)
-- @return array of Agent instances
function Agent.find_by_base_key(base_key)
    local result = {}
    for key, agent in pairs(agents) do
        if key == base_key or key:sub(1, #base_key + 1) == base_key .. "-" then
            -- Verify the suffix part is a number (avoid matching base keys that
            -- happen to share a prefix, e.g. "owner-repo-1" vs "owner-repo-10")
            if key == base_key then
                result[#result + 1] = agent
            else
                local suffix = key:sub(#base_key + 1)
                if suffix:match("^%-(%d+)$") then
                    result[#result + 1] = agent
                end
            end
        end
    end
    return result
end

--- Find agents by metadata key-value pair.
-- @param key string Metadata key to match
-- @param value any Value to match
-- @return array of Agent instances
function Agent.find_by_meta(key, value)
    local result = {}
    for _, agent in ipairs(Agent.list()) do
        if agent.metadata and agent.metadata[key] == value then
            result[#result + 1] = agent
        end
    end
    return result
end

--- Drain an agent's inbox, discarding expired messages.
-- Returns all non-expired messages and clears the inbox.
-- Messages with no expires_at are kept indefinitely.
-- @param agent_id string Agent key
-- @return array of envelope tables (may be empty), or nil if agent not found
function Agent.receive_messages(agent_id)
    local agent = Agent.get(agent_id)
    if not agent then return nil end

    local now = os.time()
    local valid = {}
    for _, envelope in ipairs(agent._inbox or {}) do
        if not envelope.expires_at or envelope.expires_at >= now then
            valid[#valid + 1] = envelope
        end
    end

    agent._inbox = {}
    return valid
end

--- Compute the next available instance suffix for a base key.
-- Returns nil if no agent exists with this base key (first instance),
-- or "-N" where N is the next available number.
-- @param base_key string The base agent key
-- @return string|nil The instance suffix (nil, "-2", "-3", ...)
function Agent.next_instance_suffix(base_key)
    local existing = Agent.find_by_base_key(base_key)
    if #existing == 0 then
        return nil
    end
    -- Find highest existing suffix number
    local max_n = 1 -- the first agent (no suffix) counts as 1
    for _, agent in ipairs(existing) do
        local key = agent:agent_key()
        if key == base_key then
            -- first instance, number = 1
        else
            local n = tonumber(key:sub(#base_key + 2)) -- skip the "-"
            if n and n > max_n then
                max_n = n
            end
        end
    end
    return "-" .. tostring(max_n + 1)
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
