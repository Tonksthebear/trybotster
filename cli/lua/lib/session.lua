-- Session base class for managing a single PTY session.
--
-- Base class for Agent and Accessory. Owns:
-- - Session UUID generation (sess-{epoch}-{seq}-{rand} format)
-- - BOTSTER_SESSION_UUID env injection into PTY env
-- - Session manifest sync to workspace store
-- - Workspace manifest sync
-- - PTY session lifecycle (spawn, close)
-- - Metadata key-value store (set_meta, get_meta)
-- - Environment variable building (build_env)
-- - Core identity fields
-- - Session registry (keyed by session_uuid)
-- - Broker scrollback replay
--
-- Subclasses (Agent, Accessory) call Session._init(self, config) for shared
-- initialization, then add type-specific fields.
--
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local hooks = require("hub.hooks")

local Session = state.class("Session")

-- Session registry keyed by session_uuid (persistent across reloads)
local sessions = state.get("agent_registry", {})

local function is_ghost_entry(entry)
    return type(entry) == "table" and entry._is_ghost == true
end

-- Sequential port counter for forward_port sessions (persistent across reloads)
local port_state = state.get("agent_port_state", { next_port = 8080 })

-- =============================================================================
-- UUID Generation
-- =============================================================================

-- Monotonic counter for collision-safe UUID generation (persistent across reloads)
local uuid_state = state.get("agent_uuid_counter", { seq = 0 })

--- Generate a collision-safe session UUID.
-- Format: "sess-{epoch}-{seq}-{random128}"
-- Combines second-level time + process-local monotonic counter + 128 bits of
-- randomness (4 independent draws). The counter alone prevents collisions under
-- burst creation; the random salt prevents collisions across process restarts
-- that might reset the counter.
-- @return string
local function generate_session_uuid()
    uuid_state.seq = uuid_state.seq + 1
    return string.format("sess-%d-%04x-%08x%08x%08x%08x",
        os.time(),
        uuid_state.seq,
        math.random(0, 0xFFFFFFFF),
        math.random(0, 0xFFFFFFFF),
        math.random(0, 0xFFFFFFFF),
        math.random(0, 0xFFFFFFFF))
end

--- Default display name for anonymous orchestration workspaces.
-- Keeps workspace identity stable via workspace_id while making UI labels readable.
-- @param branch_name string|nil
-- @return string
local function default_workspace_name(branch_name)
    if branch_name and branch_name ~= "" then
        return branch_name
    end
    return "General"
end

-- =============================================================================
-- Shared Initialization
-- =============================================================================

--- Initialize shared session fields and spawn the PTY.
-- Called by subclass constructors (Agent.new, Accessory.new).
-- self must already have its metatable set by the subclass.
--
-- Config table:
--   repo            string   (required)  "owner/repo"
--   branch_name     string   (required)
--   worktree_path   string   (required)
--   session_type    string   (optional)  "agent" (default) or "accessory"
--   session         table    (required)  single session config:
--                              { name, command, init_script, notifications, forward_port }
--   prompt          string   (optional)  task description
--   metadata        table    (optional)  plugin key-value store (e.g., issue_number, invocation_url)
--   workspace       string   (optional)  workspace name (e.g. "owner/repo#42")
--   workspace_id    string   (optional)  pre-resolved workspace ID
--   workspace_metadata table (optional)  plugin data stored on workspace manifest
--   env             table    (optional)  base environment variables
--   dims            table    (optional)  { rows = 24, cols = 80 }
--   agent_key       string   (optional)  display key (derived from repo+branch if not set)
--   agent_name      string   (optional)  config agent name (e.g., "claude")
--   profile_name    string   (optional)  DEPRECATED alias for agent_name
--
-- @param self The instance (metatable already set by subclass)
-- @param config Table of session configuration
function Session._init(self, config)
    assert(config.repo, "Session._init requires config.repo")
    assert(config.branch_name, "Session._init requires config.branch_name")
    assert(config.worktree_path, "Session._init requires config.worktree_path")
    assert(config.session, "Session._init requires config.session")

    local session_type = config.session_type or "agent"
    local session_config = config.session
    local session_name = session_config.name or session_type
    local session_uuid = generate_session_uuid()

    -- Build metadata table
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

    -- Workspace name can be explicitly provided, or inferred from a supplied workspace_id.
    local workspace_name = config.workspace
    local pre_resolved_workspace_id = config.workspace_id

    -- Set all shared fields on self
    self.session_uuid = session_uuid
    self.session_type = session_type
    self.session_name = session_name
    self._agent_key = config.agent_key
    self.repo = config.repo
    self.branch_name = config.branch_name
    self.worktree_path = config.worktree_path
    self.prompt = config.prompt
    self.metadata = metadata
    self._workspace_name = workspace_name
    self._workspace_metadata = config.workspace_metadata or {}
    self.agent_name = config.agent_name or config.profile_name
    self.profile_name = config.agent_name or config.profile_name  -- backward compat alias
    self.created_at = os.time()
    self.status = "running"
    self.title = nil          -- window title from OSC 0/2 (set by pty_title_changed hook)
    self.cwd = nil            -- current working directory from OSC 7 (set by pty_cwd_changed hook)
    self.notification = false -- true when OSC notification fired, cleared by client
    self.session = nil        -- single PtySessionHandle
    self._session_config = session_config  -- original session config from creation

    local key = self:agent_key()

    local git_path = config.worktree_path .. "/.git"
    local is_worktree = fs.exists(git_path) and not fs.is_dir(git_path)
    self._is_worktree = is_worktree

    -- Resolve device data_dir for workspace store.
    -- Subclass .new() receives a `config` parameter that shadows the global `config`,
    -- so access the global via _G to get the actual device data directory.
    local data_dir = _G.config and _G.config.data_dir and _G.config.data_dir() or nil
    self._data_dir = data_dir

    -- Build environment variables
    local env = self:build_env(config.env)
    self.hub_socket = env.BOTSTER_HUB_SOCKET
    self.hub_manifest_path = env.BOTSTER_HUB_MANIFEST_PATH

    -- Initialize Central Session Store.
    if data_dir then
        local ws = require("lib.workspace_store")
        ws.init_dir(data_dir)

        local workspace_id = pre_resolved_workspace_id
        if workspace_id then
            local existing = ws.read_workspace(data_dir, workspace_id)
            if existing then
                workspace_name = workspace_name or existing.name
            else
                log.warn(string.format("Workspace %s not found; creating standalone workspace", tostring(workspace_id)))
                workspace_id = nil
            end
        end

        if not workspace_id and workspace_name then
            local ok_ws, ws_id = pcall(function()
                local id = ws.ensure_workspace(data_dir, {
                    name = workspace_name,
                    metadata = self._workspace_metadata,
                    created_at = os.date("!%Y-%m-%dT%H:%M:%SZ", self.created_at),
                })
                return id
            end)
            if ok_ws then
                workspace_id = ws_id
            else
                log.warn(string.format("Failed to ensure workspace manifest: %s", tostring(ws_id)))
            end
        end

        -- Standalone session: allocate an anonymous orchestration workspace.
        if not workspace_id then
            if not workspace_name or workspace_name == "" then
                workspace_name = default_workspace_name(config.branch_name)
            end
            workspace_id = ws.generate_workspace_id()
            local anon_manifest = {
                id         = workspace_id,
                name       = workspace_name,
                status     = "active",
                created_at = os.date("!%Y-%m-%dT%H:%M:%SZ", self.created_at),
                updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()),
                metadata   = {},
            }
            pcall(ws.write_workspace, data_dir, workspace_id, anon_manifest)
        end

        self._workspace_id = workspace_id
        self._workspace_name = workspace_name
        self:_sync_workspace_manifest()
        self:_sync_session_manifest()
        ws.append_event(data_dir, self._workspace_id, session_uuid, "created")
    end

    -- Determine dimensions
    local rows = 24
    local cols = 80
    if config.dims then
        rows = config.dims.rows or 24
        cols = config.dims.cols or 80
    end

    -- Spawn the single PTY session
    -- Shallow-copy env for session-specific overrides
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
        session_name = session_name,
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

    local ok, handle, broker_session_id = pcall(hub.spawn_pty_with_broker, spawn_config, session_uuid)
    if not ok or not handle or not broker_session_id then
        error(string.format(
            "Failed to spawn broker-backed PTY for %s: %s",
            key,
            tostring(handle)
        ))
    end

    self.session = handle
    self._port = port

    log.info(string.format("Session %s: spawned '%s' (uuid=%s, type=%s)", key, session_name, session_uuid, session_type))

    -- Register with HandleCache via hub.register_session()
    local reg_ok, reg_index = pcall(hub.register_session, session_uuid, handle, {
        session_type = session_type,
        agent_key = key,
        workspace_id = self._workspace_id,
        broker_session_id = broker_session_id,
    })
    if reg_ok then
        log.info(string.format("Session %s: registered with HandleCache index %s",
            key, tostring(reg_index)))
    else
        log.error(string.format("Session %s: failed to register: %s", key, tostring(reg_index)))
    end

    self:set_meta("broker_session_id", tostring(broker_session_id))
    -- Store PTY dimensions so ghost PTYs use real terminal size
    local dims_ok, dim_rows, dim_cols = pcall(function() return handle:dimensions() end)
    if dims_ok and dim_rows then
        self:set_meta("broker_pty_rows", tostring(dim_rows))
        self:set_meta("broker_pty_cols", tostring(dim_cols))
    end
    log.info(string.format("Session %s: registered with broker → session %d",
        key, broker_session_id))

    -- PTY recovery source-of-truth is broker snapshot state. No file tee arming.

    -- Register in session registry (keyed by session_uuid)
    sessions[session_uuid] = self

    -- Notify observers
    hooks.notify("after_agent_create", self)

    log.info(string.format("Session created: %s (uuid=%s, type=%s)", key, session_uuid, session_type))
end

-- =============================================================================
-- Instance Methods
-- =============================================================================

--- Generate the agent key (display label).
-- Format: repo-name-branch_name[-N] (slashes replaced with dashes)
-- @return string agent key
function Session:agent_key()
    if self._agent_key then
        return self._agent_key
    end
    -- Fallback: derive from repo + branch_name
    local repo_safe = self.repo:gsub("/", "-")
    local branch_safe = self.branch_name:gsub("/", "-")
    return repo_safe .. "-" .. branch_safe
end

--- Set a metadata value and sync session manifest.
-- @param key string Metadata key
-- @param value any Metadata value
function Session:set_meta(key, value)
    self.metadata[key] = value
    self:_sync_session_manifest()
end

--- Get a metadata value.
-- @param key string Metadata key
-- @return any Metadata value or nil
function Session:get_meta(key)
    return self.metadata[key]
end

--- Sync the Central Session Store session manifest.
function Session:_sync_session_manifest()
    if not self._data_dir or not self._workspace_id then return end
    local ws = require("lib.workspace_store")

    -- Collect broker session info from metadata (single session, no indices)
    local broker_sessions = {}
    local pty_dimensions  = {}
    local sid = self.metadata["broker_session_id"]
    if sid then
        broker_sessions["0"] = tonumber(sid)
        local dim_rows = tonumber(self.metadata["broker_pty_rows"])
        local dim_cols = tonumber(self.metadata["broker_pty_cols"])
        if dim_rows and dim_cols then
            pty_dimensions["0"] = { rows = dim_rows, cols = dim_cols }
        end
    end

    -- Build plugin metadata for the manifest, excluding internal broker keys
    -- that are already represented as structured fields above.
    local plugin_metadata = {}
    local internal_keys = {
        broker_session_id = true,
        broker_pty_rows = true,
        broker_pty_cols = true,
        tee_log_path = true,
    }
    for k, v in pairs(self.metadata) do
        if not internal_keys[k] then
            plugin_metadata[k] = v
        end
    end

    local manifest = {
        uuid          = self.session_uuid,
        workspace_id  = self._workspace_id,
        agent_key     = self:agent_key(),
        type          = self.session_type,
        role          = "developer",
        repo          = self.repo,
        branch        = self.branch_name,
        worktree_path = self.worktree_path,
        agent_name    = self.agent_name,
        profile_name  = self.profile_name,  -- backward compat
        prompt        = self.prompt,  -- task description (read by `botster context prompt`)
        metadata      = plugin_metadata,   -- flattened by `botster context` for template access
        hub_id             = hub.hub_id() or hub.server_id(),
        hub_manifest_path  = self.hub_manifest_path,
        status        = (self.status == "running") and "active" or self.status,
        broker_sessions = broker_sessions,
        pty_dimensions  = pty_dimensions,
        created_at    = os.date("!%Y-%m-%dT%H:%M:%SZ", self.created_at),
        updated_at    = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()),
    }

    local ok, err = pcall(ws.write_session,
        self._data_dir, self._workspace_id, self.session_uuid, manifest)
    if not ok then
        log.warn(string.format("Failed to sync session manifest: %s", tostring(err)))
        return
    end
    pcall(ws.refresh_workspace_status, self._data_dir, self._workspace_id)
end

--- Sync the Central Session Store workspace manifest.
function Session:_sync_workspace_manifest()
    if not self._data_dir or not self._workspace_id then return end
    local ws = require("lib.workspace_store")
    local current = ws.read_workspace(self._data_dir, self._workspace_id) or {}
    local merged_metadata = {}
    for k, v in pairs(current.metadata or {}) do
        merged_metadata[k] = v
    end
    for k, v in pairs(self._workspace_metadata or {}) do
        merged_metadata[k] = v
    end

    local manifest = {
        id         = self._workspace_id,
        name       = self._workspace_name or current.name or default_workspace_name(self.branch_name),
        status     = current.status or "active",
        created_at = current.created_at or os.date("!%Y-%m-%dT%H:%M:%SZ", self.created_at),
        updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()),
        metadata   = merged_metadata,
    }

    local ok, err = pcall(ws.write_workspace, self._data_dir, self._workspace_id, manifest)
    if not ok then
        log.warn(string.format("Failed to sync workspace manifest: %s", tostring(err)))
        return
    end
    pcall(ws.refresh_workspace_status, self._data_dir, self._workspace_id)
end

--- Move this live session into a different workspace.
-- Creates the target workspace when only workspace_name is provided.
-- @param opts table { workspace_id, workspace_name, workspace_metadata }
-- @return table|nil result
-- @return string|nil error
function Session:move_to_workspace(opts)
    opts = opts or {}
    if not self._data_dir then
        return nil, "No data_dir configured for workspace operations"
    end

    local ws = require("lib.workspace_store")
    local target_workspace_id = opts.workspace_id
    local target_workspace_name = opts.workspace_name
    local target_workspace_metadata = opts.workspace_metadata or {}

    if target_workspace_id == "" then target_workspace_id = nil end
    if target_workspace_name == "" then target_workspace_name = nil end

    if not target_workspace_id and not target_workspace_name then
        return nil, "workspace_id or workspace_name is required"
    end

    local now = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
    local old_workspace_id = self._workspace_id
    local old_workspace_name = self._workspace_name
    local workspace_id = target_workspace_id
    local workspace_name = target_workspace_name

    if workspace_id then
        local target_manifest = ws.read_workspace(self._data_dir, workspace_id)
        if not target_manifest then
            return nil, string.format("Workspace '%s' not found", tostring(workspace_id))
        end

        if workspace_name and workspace_name ~= target_manifest.name then
            local renamed = ws.rename_workspace(self._data_dir, workspace_id, workspace_name)
            if renamed then
                target_manifest.name = workspace_name
            else
                return nil, string.format("Failed to rename workspace '%s'", tostring(workspace_id))
            end
        end
        workspace_name = workspace_name or target_manifest.name or default_workspace_name(self.branch_name)
    else
        local created_id, created_manifest = ws.ensure_workspace(self._data_dir, {
            name = workspace_name,
            metadata = target_workspace_metadata,
            created_at = now,
        })
        if not created_id then
            return nil, string.format("Failed to create workspace '%s'", tostring(workspace_name))
        end
        workspace_id = created_id
        workspace_name = created_manifest and created_manifest.name or workspace_name
    end

    if old_workspace_id == workspace_id and old_workspace_name == workspace_name then
        return {
            workspace_id = workspace_id,
            workspace_name = workspace_name,
            previous_workspace_id = old_workspace_id,
            previous_workspace_name = old_workspace_name,
            changed = false,
        }, nil
    end

    -- Keep metadata in sync for plugins that look up by workspace fields.
    self.metadata = self.metadata or {}
    self.metadata.workspace_id = workspace_id
    self.metadata.workspace = workspace_name

    -- Merge any workspace metadata updates onto this session.
    self._workspace_metadata = self._workspace_metadata or {}
    for k, v in pairs(target_workspace_metadata) do
        self._workspace_metadata[k] = v
    end

    self._workspace_id = workspace_id
    self._workspace_name = workspace_name

    -- Update HandleCache metadata so Rust-side lookups see the new workspace_id.
    if self.session then
        local sid = tonumber(self.metadata["broker_session_id"])
        if sid then
            local ok_reg, reg_err = pcall(hub.register_session, self.session_uuid, self.session, {
                session_type = self.session_type,
                agent_key = self:agent_key(),
                workspace_id = workspace_id,
                broker_session_id = sid,
            })
            if not ok_reg then
                log.warn(string.format("Session %s: failed to refresh HandleCache workspace: %s",
                    self:agent_key(), tostring(reg_err)))
            end
        end
    end

    self:_sync_workspace_manifest()
    self:_sync_session_manifest()
    ws.append_event(self._data_dir, workspace_id, self.session_uuid, "moved")

    if old_workspace_id and old_workspace_id ~= workspace_id then
        local old_manifest = ws.read_session(self._data_dir, old_workspace_id, self.session_uuid)
        if old_manifest then
            old_manifest.status = "closed"
            old_manifest.updated_at = now
            old_manifest.moved_to_workspace_id = workspace_id
            old_manifest.moved_to_workspace_name = workspace_name
            pcall(ws.write_session, self._data_dir, old_workspace_id, self.session_uuid, old_manifest)
            ws.append_event(self._data_dir, old_workspace_id, self.session_uuid, "moved")
        end
        pcall(ws.refresh_workspace_status, self._data_dir, old_workspace_id)
    end
    pcall(ws.refresh_workspace_status, self._data_dir, workspace_id)

    return {
        workspace_id = workspace_id,
        workspace_name = workspace_name,
        previous_workspace_id = old_workspace_id,
        previous_workspace_name = old_workspace_name,
        changed = true,
    }, nil
end

--- Close the session and clean up resources.
-- @param delete_worktree boolean Whether to queue worktree deletion
function Session:close(delete_worktree)
    local key = self:agent_key()

    -- Notify observers
    hooks.notify("before_agent_close", self)

    -- Unregister from HandleCache
    local ok, err = pcall(hub.unregister_session, self.session_uuid)
    if not ok then
        log.warn(string.format("Session %s: failed to unregister: %s", key, tostring(err)))
    end

    -- Kill the PTY session
    if self.session then
        local ok2, err2 = pcall(function() self.session:kill() end)
        if not ok2 then
            log.warn(string.format("Session %s: error killing PTY: %s", key, tostring(err2)))
        end
    end
    self.session = nil
    self.status = "closed"

    -- Remove from registry
    sessions[self.session_uuid] = nil

    -- Mark the Central Session Store session as closed
    if self._data_dir and self._workspace_id then
        local ws = require("lib.workspace_store")
        local manifest = ws.read_session(self._data_dir, self._workspace_id, self.session_uuid)
        if manifest then
            manifest.status     = "closed"
            manifest.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
            pcall(ws.write_session,
                self._data_dir, self._workspace_id, self.session_uuid, manifest)
            pcall(ws.refresh_workspace_status, self._data_dir, self._workspace_id)
        end
    end

    -- Queue worktree deletion if requested
    if delete_worktree then
        local ok3, err3 = pcall(worktree.delete, self.worktree_path, self.branch_name)
        if not ok3 then
            log.warn(string.format("Session %s: failed to delete worktree: %s",
                key, tostring(err3)))
        end
    end

    -- Notify observers
    hooks.notify("after_agent_close", self)

    log.info(string.format("Session closed: %s (uuid=%s, delete_worktree=%s)",
        key, self.session_uuid, tostring(delete_worktree or false)))
end

--- Replay broker ring-buffer scrollback into the session's shadow screen.
function Session:replay_broker_scrollback()
    local key = self:agent_key()
    local session_id = tonumber(self:get_meta("broker_session_id"))
    if not session_id then return end

    local snapshot = hub.get_pty_snapshot_from_broker(session_id)
    if snapshot and #snapshot > 0 and self.session then
        local ok, err = pcall(function() self.session:feed_output(snapshot) end)
        if ok then
            log.info(string.format("Session %s: replayed %d bytes of broker scrollback",
                key, #snapshot))
        else
            log.warn(string.format("Session %s: failed to replay scrollback: %s",
                key, tostring(err)))
        end
    end
end

--- Build environment variables for spawned sessions.
-- @param base_env table Optional base env vars to merge
-- @return table Environment variables
function Session:build_env(base_env)
    local env = {}
    if base_env then
        for k, v in pairs(base_env) do
            env[k] = v
        end
    end
    env.TERM = env.TERM or os.getenv("TERM") or "xterm-256color"
    env.BOTSTER_WORKTREE_PATH = self.worktree_path
    env.BOTSTER_AGENT_KEY = self:agent_key()
    env.BOTSTER_SESSION_UUID = self.session_uuid
    env.BOTSTER_HUB_ID = hub.server_id() or ""
    local local_hub_id = hub.hub_id and hub.hub_id() or nil
    if local_hub_id and hub_discovery and hub_discovery.socket_path then
        local ok, socket_path = pcall(hub_discovery.socket_path, local_hub_id)
        if ok and type(socket_path) == "string" and socket_path ~= "" then
            env.BOTSTER_HUB_SOCKET = socket_path
        end
    end
    if local_hub_id and hub_discovery and hub_discovery.manifest_path then
        local ok, manifest_path = pcall(hub_discovery.manifest_path, local_hub_id)
        if ok and type(manifest_path) == "string" and manifest_path ~= "" then
            env.BOTSTER_HUB_MANIFEST_PATH = manifest_path
        end
    end
    if self.prompt and self.prompt ~= "" then
        env.BOTSTER_PROMPT = self.prompt
    end
    -- Fire filter hook for customization
    env = hooks.call("filter_agent_env", env, self) or env
    return env
end

--- Get session metadata for clients.
-- Returns a serializable table of session info.
-- @return table Session info
function Session:info()
    local key = self:agent_key()

    local port = self._port

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
        session_uuid = self.session_uuid,
        session_type = self.session_type,
        session_name = self.session_name,
        display_name = display_name,
        title = self.title,
        cwd = self.cwd,
        agent_name = self.agent_name,
        profile_name = self.profile_name,  -- backward compat
        repo = self.repo,
        metadata = self.metadata,
        workspace_name = self._workspace_name,
        workspace_id = self._workspace_id,
        branch_name = self.branch_name,
        worktree_path = self.worktree_path,
        in_worktree = self._is_worktree or false,
        status = self.status,
        notification = self.notification or false,
        port = port,
        created_at = self.created_at,
    }
end

-- =============================================================================
-- Module-Level Functions (on the Session class table)
-- =============================================================================

--- Get a session by session_uuid (primary lookup).
-- @param session_uuid string Session UUID
-- @return Session subclass instance or nil
function Session.get(session_uuid)
    local entry = sessions[session_uuid]
    if is_ghost_entry(entry) then
        return nil
    end
    return entry
end

--- Find a session by its agent_key (display label).
-- @param key string Agent key
-- @return Session subclass instance or nil
function Session.find_by_agent_key(key)
    for _, sess in ipairs(Session.list()) do
        if sess:agent_key() == key then
            return sess
        end
    end
    return nil
end

--- List all sessions in creation order.
-- @return array of Session subclass instances
function Session.list()
    local result = {}
    for _, sess in pairs(sessions) do
        if not is_ghost_entry(sess) then
            table.insert(result, sess)
        end
    end
    -- Sort by creation time for stable ordering
    table.sort(result, function(a, b)
        return (a.created_at or 0) < (b.created_at or 0)
    end)
    return result
end

--- Find all sessions matching a base key (ignoring instance suffix).
-- @param base_key string The base agent key (without instance suffix)
-- @return array of Session subclass instances
function Session.find_by_base_key(base_key)
    local result = {}
    for _, sess in ipairs(Session.list()) do
        local key = sess:agent_key()
        if key == base_key then
            result[#result + 1] = sess
        elseif key:sub(1, #base_key + 1) == base_key .. "-" then
            local suffix = key:sub(#base_key + 1)
            if suffix:match("^%-(%d+)$") then
                result[#result + 1] = sess
            end
        end
    end
    return result
end

--- Find sessions by metadata key-value pair.
-- @param key string Metadata key to match
-- @param value any Value to match
-- @return array of Session subclass instances
function Session.find_by_meta(key, value)
    local result = {}
    for _, sess in ipairs(Session.list()) do
        if sess.metadata and sess.metadata[key] == value then
            result[#result + 1] = sess
        end
    end
    return result
end

--- Find all running sessions matching a workspace name.
-- @param name string  Workspace name (e.g. "owner/repo#42")
-- @return array of Session subclass instances
function Session.find_by_workspace(name)
    local result = {}
    for _, sess in ipairs(Session.list()) do
        if sess._workspace_name == name then
            result[#result + 1] = sess
        end
    end
    return result
end

--- Compute the next available instance suffix for a base key.
-- @param base_key string The base agent key
-- @return string|nil The instance suffix (nil, "-2", "-3", ...)
function Session.next_instance_suffix(base_key)
    local existing = Session.find_by_base_key(base_key)
    if #existing == 0 then
        return nil
    end
    local max_n = 1
    for _, sess in ipairs(existing) do
        local agent_key = sess:agent_key()
        if agent_key ~= base_key then
            local n = tonumber(agent_key:sub(#base_key + 2))
            if n and n > max_n then
                max_n = n
            end
        end
    end
    return "-" .. tostring(max_n + 1)
end

--- Count active sessions.
-- @return number
function Session.count()
    local count = 0
    for _, sess in pairs(sessions) do
        if not is_ghost_entry(sess) then
            count = count + 1
        end
    end
    return count
end

--- Register a recovered ghost session info entry.
-- Ghost entries share the same `sessions` registry and are replaced
-- automatically when the real session with the same UUID is created.
-- @param ghost_info table Session-info style table
-- @return boolean
function Session.register_ghost(ghost_info)
    if type(ghost_info) ~= "table" then return false end
    if not ghost_info.session_uuid or ghost_info.session_uuid == "" then return false end
    if not ghost_info.id or ghost_info.id == "" then return false end
    ghost_info._is_ghost = true
    ghost_info.created_at = ghost_info.created_at or os.time()
    sessions[ghost_info.session_uuid] = ghost_info
    return true
end

--- Get info tables for all sessions (for client broadcast).
-- @return array of info tables sorted by creation time
function Session.all_info()
    local result = {}
    for _, entry in pairs(sessions) do
        if is_ghost_entry(entry) then
            local info = {}
            for k, v in pairs(entry) do
                if k ~= "_is_ghost" then
                    info[k] = v
                end
            end
            result[#result + 1] = info
        else
            local info = entry:info()
            result[#result + 1] = info
        end
    end
    -- Stable client-facing ordering.
    table.sort(result, function(a, b)
        local ac = tonumber(a.created_at) or 0
        local bc = tonumber(b.created_at) or 0
        if ac == bc then
            return tostring(a.id or "") < tostring(b.id or "")
        end
        return ac < bc
    end)
    return result
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================

function Session._before_reload()
    log.info("session.lua reloading (persistent metatable -- instances auto-upgrade)")
end

function Session._after_reload()
    log.info(string.format("session.lua reloaded -- %d sessions preserved", Session.count()))
end

return Session
