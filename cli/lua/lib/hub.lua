-- Hub proxy class for transparent local/remote hub access.
--
-- Hub.get(hub_id) returns either a local hub object (direct Lua calls) or a
-- transparent remote proxy (routes through hub_client.request). Plugin
-- authors call Hub.get(params.hub_id):get_pty_snapshot(agent_id, session)
-- without caring whether the hub is local or remote.
--
-- Hub.get() auto-connects to remote hubs on demand via hub_discovery.
-- No manual registration is required from plugin code.
--
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local Agent = require("lib.agent")
local InternalClient = require("lib.internal_client")

local Hub = state.class("Hub")

-- Remote hub registry (persistent across reloads): hub_id -> conn_id
local remote_hubs = state.get("hub_remote_registry", {})

-- Local hub ID (cached on first access)
local self_id = hub.hub_id()

-- =============================================================================
-- Envelope Helpers
-- =============================================================================

--- Generate a unique message ID.
-- Format: msg_<hub_prefix>_<timestamp>_<random hex>
-- Hub prefix (first 8 chars of hub_id) scopes IDs globally so reply_to
-- threading works correctly across hubs without collision.
local function generate_msg_id()
    local hub_prefix = self_id and self_id:sub(1, 8) or "00000000"
    return string.format("msg_%s_%d_%06x", hub_prefix, os.time(), math.random(0, 0xffffff))
end

--- Build a message envelope.
-- @param from_hub_id string Sender hub ID
-- @param from_agent_id string Sender agent key
-- @param opts table { type, payload, reply_to, expires_in }
-- @return table Envelope
local function build_envelope(from_hub_id, from_agent_id, opts)
    local expires_in = opts.expires_in or 3600  -- 1 hour default
    return {
        msg_id     = generate_msg_id(),
        type       = opts.type or "message",
        reply_to   = opts.reply_to,
        from       = { hub_id = from_hub_id, agent_id = from_agent_id },
        payload    = opts.payload,
        expires_at = os.time() + expires_in,
    }
end

local function dispatch_local_command(command)
    command.request_id = command.request_id or generate_msg_id()
    return InternalClient.dispatch("hub_proxy", command)
end

local function session_payload(session)
    return require("lib.client_session_payload").build(session, Agent.all_info())
end

local function response_for(dispatch_result, request_id)
    for _, frame in ipairs(dispatch_result.frames or {}) do
        if frame.type == "command_response" and frame.request_id == request_id then
            return frame
        end
    end
    return nil
end

local function dispatch_local_command_response(command)
    command.request_id = command.request_id or generate_msg_id()
    local request_id = command.request_id
    local result = dispatch_local_command(command)
    local response = response_for(result, request_id)
    if response and response.ok == false then
        error(response.error or "command failed")
    end
    return response or { ok = true }
end

-- =============================================================================
-- Constructor (internal — use Hub.get())
-- =============================================================================

--- Create a Hub instance.
-- @param hub_id string Hub identifier
-- @param is_local boolean Whether this is the local hub
-- @param conn_id string|nil Connection ID for remote hubs
-- @return Hub instance
local function new_hub(hub_id, is_local, conn_id)
    return setmetatable({
        id = hub_id,
        _is_local = is_local,
        _conn_id = conn_id,
    }, Hub)
end

-- =============================================================================
-- Public API — Registry
-- =============================================================================

--- Register a remote hub connection.
-- Called by the orchestrator plugin when it connects to a remote hub.
-- @param hub_id string Remote hub identifier
-- @param conn_id string hub_client connection ID
function Hub.register(hub_id, conn_id)
    remote_hubs[hub_id] = conn_id
    log.info(string.format("Hub.register: %s -> %s", hub_id, conn_id))
end

--- Unregister a remote hub connection and close the underlying socket.
-- @param hub_id string Remote hub identifier
function Hub.unregister(hub_id)
    local conn_id = remote_hubs[hub_id]
    if conn_id then
        pcall(hub_client.close, conn_id)
    end
    remote_hubs[hub_id] = nil
    log.info(string.format("Hub.unregister: %s", hub_id))
end

--- Remove connections to hubs that are no longer running.
-- Call periodically to clean up stale auto-connections.
-- NOTE: Lua (via next()) permits nil-ing the current key during pairs() traversal.
-- PiL: "the only safe assumption is that you can delete the field currently being visited."
-- Hub.unregister() sets remote_hubs[hub_id] = nil for the key being visited — safe.
function Hub.cleanup_dead()
    for hub_id, _ in pairs(remote_hubs) do
        if not hub_discovery.is_running(hub_id) then
            log.info(string.format("Hub.cleanup_dead: hub %s no longer running, unregistering", hub_id))
            Hub.unregister(hub_id)
        end
    end
end

-- =============================================================================
-- Public API — Hub.get()
-- =============================================================================

--- Get a Hub object by ID.
-- Returns a local hub if hub_id is nil or matches self, otherwise a remote proxy.
-- Auto-connects to unknown remote hubs via hub_discovery on first access.
-- @param hub_id string|nil Hub identifier (nil = local)
-- @return Hub instance
function Hub.get(hub_id)
    -- nil or self -> local
    if not hub_id or hub_id == self_id then
        return new_hub(self_id, true, nil)
    end

    -- Already connected remote hub
    local conn_id = remote_hubs[hub_id]
    if conn_id then
        return new_hub(hub_id, false, conn_id)
    end

    -- Auto-connect: look up socket path via hub_discovery
    local socket_path = hub_discovery.socket_path and hub_discovery.socket_path(hub_id)
    if not socket_path then
        error(string.format("Hub.get: hub '%s' not found or not running", hub_id))
    end

    log.info(string.format("Hub.get: auto-connecting to hub '%s'", hub_id))
    local new_conn_id = hub_client.connect(socket_path)
    remote_hubs[hub_id] = new_conn_id
    -- Emit for observability; other plugins can listen if needed
    events.emit("hub_connected", { hub_id = hub_id, conn_id = new_conn_id })

    return new_hub(hub_id, false, new_conn_id)
end

--- Check if a hub ID refers to the local hub.
-- @param hub_id string|nil Hub identifier
-- @return boolean
function Hub.is_local(hub_id)
    return not hub_id or hub_id == self_id
end

--- Call fn safely, detecting and cleaning up dead remote connections.
-- On local hubs, calls fn() directly with no error wrapping.
-- On remote hubs, catches connection errors (timeout, closed, broken pipe)
-- and unregisters the hub so Hub.get() can auto-reconnect on next call.
-- @param hub_id string|nil Hub identifier (nil = local)
-- @param fn function The function to call (no args)
-- @return any Return value of fn()
function Hub.call_safely(hub_id, fn)
    if Hub.is_local(hub_id) then
        return fn()
    end
    local ok, result = pcall(fn)
    if not ok then
        local err = tostring(result)
        if err:find("timeout", 1, true) or err:find("connection", 1, true)
                or err:find("closed", 1, true) or err:find("broken pipe", 1, true) then
            log.warn(string.format("Hub.call_safely: hub %s connection appears dead (%s), unregistering",
                hub_id, err))
            Hub.unregister(hub_id)
        end
        error(result)
    end
    return result
end

-- =============================================================================
-- Instance Methods
-- =============================================================================

--- Get a PTY snapshot from an agent session.
-- Local: calls Agent directly. Remote: uses hub_client.request().
-- @param agent_id string Agent key or session_uuid
-- @param session string|nil Session name (ignored in single-PTY model, kept for API compat)
-- @return string Snapshot content
function Hub:get_pty_snapshot(agent_id, session)
    session = session or "agent"

    if self._is_local then
        local agent = Agent.get(agent_id)
        if not agent then
            error(string.format("Hub:get_pty_snapshot: agent '%s' not found", agent_id))
        end
        if not agent.session then
            error(string.format("Hub:get_pty_snapshot: no PTY session on agent '%s'", agent_id))
        end
        return agent.session:get_screen()
    end

    -- Remote: blocking request via hub_client.request()
    local result = hub_client.request(self._conn_id, {
        type = "get_pty_snapshot",
        agent_id = agent_id,
        session = session,
    }, 10000)

    if result.error then
        error(string.format("Hub:get_pty_snapshot remote error: %s", result.error))
    end

    return result.result
end

--- Send a message to an agent's PTY session.
-- Local: calls send_message directly. Remote: uses hub_client.request().
-- @param agent_id string Agent key or session_uuid
-- @param text string Message text to deliver
-- @param session string|nil Session name (ignored in single-PTY model, kept for API compat)
function Hub:send_message(agent_id, text, session)
    session = session or "agent"

    if self._is_local then
        local agent = Agent.get(agent_id)
        if not agent then
            error(string.format("Hub:send_message: agent '%s' not found", agent_id))
        end
        if not agent.session then
            error(string.format("Hub:send_message: no PTY session on agent '%s'", agent_id))
        end
        agent.session:send_message(text)
        return "Message sent"
    end

    local result = hub_client.request(self._conn_id, {
        type = "send_message",
        agent_id = agent_id,
        session = session,
        text = text,
    }, 10000)

    if result.error then
        error(string.format("Hub:send_message remote error: %s", result.error))
    end

    return result.result
end

--- Post a structured message to an agent's inbox.
-- Builds a full envelope (msg_id, from, expires_at) and writes it to the
-- agent's inbox. Fires a PTY doorbell so the agent knows to call receive_messages().
-- For type="notify", skips inbox and writes text directly to PTY instead.
-- Local: writes inbox directly. Remote: RPC to target hub.
-- @param agent_id string Agent key or session_uuid
-- @param opts table { type, payload, reply_to, expires_in, session, from_agent_id }
-- @return table { msg_id, status }
function Hub:post(agent_id, opts)
    opts = opts or {}
    local msg_type = opts.type or "message"

    if self._is_local then
        local agent = Agent.get(agent_id)
        if not agent then
            error(string.format("Hub:post: agent '%s' not found", agent_id))
        end
        -- Only agents accept inbox messages; accessories have no AI to read them
        if agent.session_type ~= "agent" and msg_type ~= "notify" then
            error(string.format("Hub:post: session '%s' is an accessory, not an agent", agent_id))
        end

        if msg_type == "notify" then
            -- PTY-only: write text directly, no inbox, no doorbell
            if not agent.session then
                error(string.format("Hub:post: no PTY session on agent '%s'", agent_id))
            end
            agent.session:send_message(opts.payload or "")
            return { msg_id = nil, status = "delivered" }
        end

        -- Build envelope — hub injects msg_id and timestamps
        local envelope = build_envelope(self.id, opts.from_agent_id or "unknown", opts)

        -- Write to inbox directly
        agent._inbox = agent._inbox or {}
        table.insert(agent._inbox, envelope)

        -- PTY doorbell — minimal trigger line only, payload stays in inbox
        if agent.session then
            local sender_name = opts.from_label or envelope.from.agent_id
            agent.session:send_message(string.format(
                "\n\xe2\xac\xa1 [botster-mcp] new message from %s \xe2\x80\x94 use receive_messages() via botster MCP\n",
                sender_name
            ))
            return { msg_id = envelope.msg_id, status = "delivered" }
        end

        -- Inbox written but session was missing — message is readable via receive_messages()
        -- but agent won't see a doorbell
        log.warn(string.format("Hub:post: inbox written for %s but no PTY session, no doorbell",
            agent_id))
        return { msg_id = envelope.msg_id, status = "inbox_only" }
    end

    -- Remote hub: RPC to target hub which handles inbox write and doorbell
    local result = hub_client.request(self._conn_id, {
        type          = "post_message",
        agent_id      = agent_id,
        msg_type      = msg_type,
        payload       = opts.payload,
        reply_to      = opts.reply_to,
        expires_in    = opts.expires_in,
        session       = opts.session or "agent",
        from_hub_id   = self_id,
        from_agent_id = opts.from_agent_id or "unknown",
    }, 10000)

    if result.error then
        error(string.format("Hub:post remote error: %s", result.error))
    end

    return result.result
end

--- Drain an agent's inbox on this hub.
-- Returns all non-expired messages and clears the inbox.
-- Local: calls Agent.receive_messages() directly. Remote: uses hub_client.request().
-- NOTE: No authorization checks — any caller can drain any agent's inbox.
-- Acceptable for single-user deployments; multi-user would need caller verification.
-- @param agent_id string Agent key or session_uuid
-- @return array of envelope tables (may be empty)
function Hub:receive_messages(agent_id)
    if self._is_local then
        local messages = Agent.receive_messages(agent_id)
        if messages == nil then
            error(string.format("Hub:receive_messages: agent '%s' not found", agent_id))
        end
        return messages
    end

    local result = hub_client.request(self._conn_id, {
        type = "receive_messages",
        agent_id = agent_id,
    }, 10000)

    if result.error then
        error(string.format("Hub:receive_messages remote error: %s", result.error))
    end

    return result.result
end

local function copy_table(src)
    local out = {}
    if src then
        for k, v in pairs(src) do out[k] = v end
    end
    return out
end

local function normalize_create_agent_opts(issue_or_branch, prompt, agent_name, workspace_id, workspace_name, target)
    if type(issue_or_branch) == "table" then
        local opts = copy_table(issue_or_branch)
        opts.issue_or_branch = opts.issue_or_branch or opts.branch
        opts.metadata = copy_table(opts.metadata)
        return opts
    end

    return {
        issue_or_branch = issue_or_branch,
        prompt = prompt,
        agent_name = agent_name,
        workspace_id = workspace_id,
        workspace_name = workspace_name,
        target_id = target and target.target_id or nil,
        target_path = target and target.target_path or nil,
        target_repo = target and target.target_repo or nil,
        metadata = target and require("lib.target_context").with_metadata(nil, target) or {},
    }
end

local function create_agent_command(opts)
    local request_id = opts.request_id or (opts.metadata and opts.metadata.request_id) or generate_msg_id()
    local metadata = copy_table(opts.metadata)
    if opts.label and metadata.label == nil then
        metadata.label = opts.label
    end
    if opts.workspace_id or opts.workspace_name then
        metadata.workspace_id = opts.workspace_id or metadata.workspace_id
        metadata.workspace = opts.workspace_name or metadata.workspace
    end
    if metadata.request_id == nil then
        metadata.request_id = request_id
    end
    if opts.assignment_id and metadata.assignment_id == nil then
        metadata.assignment_id = opts.assignment_id
    end

    return {
        type = "create_agent",
        request_id = request_id,
        issue_or_branch = opts.issue_or_branch,
        branch = opts.branch,
        label = opts.label,
        prompt = opts.prompt,
        from_worktree = opts.from_worktree,
        agent_name = opts.agent_name,
        workspace_id = opts.workspace_id,
        workspace_name = opts.workspace_name,
        workspace_template = opts.workspace_template,
        invocation_url = opts.invocation_url,
        target_id = opts.target_id,
        target_path = opts.target_path,
        target_repo = opts.target_repo,
        repo = opts.repo,
        metadata = metadata,
    }
end

--- Create an agent on this hub.
-- Preferred plugin API: Hub.get():create_agent({
--   target_id = "...", target_path = "...", agent_name = "...",
--   issue_or_branch = "branch-or-ticket", label = "Worker label",
--   prompt = "...", request_id = "...",
--   assignment_id = "...", metadata = { owner_plugin = "...", ... },
-- })
-- Backward-compatible positional arguments are still accepted.
-- Local: dispatches through internal client command ingress. Remote: uses hub_client.request().
-- @return table Result payload
function Hub:create_agent(issue_or_branch, prompt, agent_name, workspace_id, workspace_name, target)
    local opts = normalize_create_agent_opts(issue_or_branch, prompt, agent_name, workspace_id, workspace_name, target)
    opts.issue_or_branch = opts.issue_or_branch or opts.branch
    local command = create_agent_command(opts)

    if self._is_local then
        local before = {}
        for _, session in ipairs(Agent.list()) do
            before[session.session_uuid] = true
        end
        local response = dispatch_local_command_response(command)
        if response.session_uuid then
            local created = Agent.get(response.session_uuid)
            if created then
                local payload = session_payload(created)
                payload.request_id = response.request_id or command.request_id
                payload.assignment_id = response.assignment_id or command.metadata.assignment_id
                return payload
            end
        end
        for _, session in ipairs(Agent.list()) do
            if not before[session.session_uuid] then
                return session_payload(session)
            end
        end
        return {
            status = response.status or "pending",
            request_id = response.request_id or command.request_id,
            assignment_id = response.assignment_id or command.metadata.assignment_id,
            message = "Agent creation initiated (worktree may be creating async)",
        }
    end

    local result = hub_client.request(self._conn_id, command, 60000)

    if result.error then
        error(string.format("Hub:create_agent remote error: %s", result.error))
    end

    return result.result
end

--- List sessions owned by a plugin.
-- @param owner_plugin string Plugin key
-- @return array of { session_uuid, label, metadata, status }
function Hub:list_owned_sessions(owner_plugin)
    if self._is_local then
        local owned = {}
        for _, session in ipairs(Agent.list()) do
            if session.owner_plugin == owner_plugin
                    or (session.metadata and session.metadata.owner_plugin == owner_plugin) then
                owned[#owned + 1] = {
                    session_uuid = session.session_uuid,
                    label = session.label,
                    metadata = session.metadata,
                    status = session.status,
                }
            end
        end
        return owned
    end

    local result = hub_client.request(self._conn_id, {
        type = "list_owned_sessions",
        owner_plugin = owner_plugin,
    }, 10000)

    if result.error then
        error(string.format("Hub:list_owned_sessions remote error: %s", result.error))
    end

    local response = result.result or {}
    if response.sessions then
        return response.sessions
    end
    return response
end

--- List workspaces on this hub.
-- Includes persisted workspace manifests and current running-session membership.
-- @return array of workspace tables
function Hub:list_workspaces()
    if self._is_local then
        local data_dir = config.data_dir and config.data_dir() or nil
        if not data_dir then
            return {}
        end

        local ws = require("lib.workspace_store")
        local workspaces = ws.list_workspaces(data_dir)
        local counts_by_id = {}

        for _, session in ipairs(Agent.all_info()) do
            local ws_id = session.workspace_id
            if ws_id then
                if not counts_by_id[ws_id] then
                    counts_by_id[ws_id] = {
                        agents = {},
                        session_counts = { agent = 0, accessory = 0, other = 0 },
                    }
                end
                local rec = counts_by_id[ws_id]
                rec.agents[#rec.agents + 1] = session.id
                if session.session_type == "agent" then
                    rec.session_counts.agent = rec.session_counts.agent + 1
                elseif session.session_type == "accessory" then
                    rec.session_counts.accessory = rec.session_counts.accessory + 1
                else
                    rec.session_counts.other = rec.session_counts.other + 1
                end
            end
        end

        local result = {}
        for _, workspace in ipairs(workspaces) do
            local counts = counts_by_id[workspace.id]
            workspace.agents = counts and counts.agents or {}
            workspace.session_counts = counts and counts.session_counts or {
                agent = 0,
                accessory = 0,
                other = 0,
            }
            -- Only return workspaces with running sessions
            if counts then
                table.insert(result, workspace)
            end
        end

        return result
    end
    error("Hub:list_workspaces is local-only; remote clients receive workspace entities")
end

--- Rename a workspace on this hub.
-- @param workspace_id string
-- @param new_name string
-- @return table
function Hub:rename_workspace(workspace_id, new_name)
    if self._is_local then
        local response = dispatch_local_command_response({
            type = "rename_workspace",
            workspace_id = workspace_id,
            new_name = new_name,
        })
        return {
            workspace_id = workspace_id,
            name = response.name or new_name,
        }
    end

    local result = hub_client.request(self._conn_id, {
        type = "rename_workspace",
        workspace_id = workspace_id,
        new_name = new_name,
    }, 10000)

    if result.error then
        error(string.format("Hub:rename_workspace remote error: %s", result.error))
    end

    return result.result
end

--- Move a live session to another workspace.
-- @param agent_id string Session UUID or agent key
-- @param workspace_id string|nil Target workspace ID
-- @param workspace_name string|nil Target workspace name
-- @return table
function Hub:move_agent_workspace(agent_id, workspace_id, workspace_name)
    if self._is_local then
        local response = dispatch_local_command_response({
            type = "move_agent_workspace",
            agent_id = agent_id,
            session_uuid = agent_id,
            workspace_id = workspace_id,
            workspace_name = workspace_name,
        })
        local session = Agent.get(response.session_uuid or agent_id)
        if not session then
            error(string.format("Hub:move_agent_workspace: session '%s' not found", tostring(agent_id)))
        end
        return {
            agent_id = session.session_uuid,
            session_uuid = session.session_uuid,
            workspace_id = response.workspace_id or session._workspace_id,
            workspace_name = response.workspace_name or session._workspace_name,
            previous_workspace_id = response.previous_workspace_id,
            previous_workspace_name = response.previous_workspace_name,
        }
    end

    local result = hub_client.request(self._conn_id, {
        type = "move_agent_workspace",
        agent_id = agent_id,
        workspace_id = workspace_id,
        workspace_name = workspace_name,
    }, 10000)

    if result.error then
        error(string.format("Hub:move_agent_workspace remote error: %s", result.error))
    end

    return result.result
end

--- Update a session's label or task on this hub.
-- @param agent_id string Session UUID or agent key
-- @param fields table { label = string|nil, task = string|nil }
-- @return table Updated session info
function Hub:update_session(agent_id, fields)
    fields = fields or {}
    if self._is_local then
        local response = dispatch_local_command_response({
            type = "update_session",
            agent_id = agent_id,
            session_uuid = agent_id,
            label = fields.label,
            task = fields.task,
        })
        local session = Agent.get(response.session_uuid or agent_id)
        if not session then
            error(string.format("Hub:update_session: session '%s' not found", tostring(agent_id)))
        end
        return session_payload(session)
    end

    local result = hub_client.request(self._conn_id, {
        type = "update_session",
        agent_id = agent_id,
        label = fields.label,
        task = fields.task,
    }, 10000)

    if result.error then
        error(string.format("Hub:update_session remote error: %s", result.error))
    end

    return result.result
end

--- Delete an agent on this hub.
-- Local: dispatches through internal client command ingress. Remote: uses hub_client.request().
-- @param agent_id string Agent key
-- @param delete_worktree boolean|nil Also delete the git worktree (default false)
-- @return string Result message
function Hub:delete_agent(agent_id, delete_worktree)
    if self._is_local then
        -- Resolve agent_label if agent_id looks like a label (no match by key)
        local resolved_id = agent_id
        local agent = Agent.get(agent_id)
        if not agent then
            -- Try label lookup
            for _, a in ipairs(Agent.list()) do
                if a.label == agent_id then
                    resolved_id = a.session_uuid
                    break
                end
            end
        end

        dispatch_local_command_response({
            type = "delete_agent",
            agent_id = resolved_id,
            session_uuid = resolved_id,
            delete_worktree = delete_worktree or false,
        })
        if not Agent.get(resolved_id) then
            return "Agent deleted: " .. resolved_id
        end
        return "Delete requested: " .. resolved_id
    end

    local result = hub_client.request(self._conn_id, {
        type = "delete_agent",
        agent_id = agent_id,
        delete_worktree = delete_worktree or false,
    }, 30000)

    if result.error then
        error(string.format("Hub:delete_agent remote error: %s", result.error))
    end

    return result.result
end

--- List sessions on the local hub.
-- @return array of agent info tables
function Hub:agent_list()
    if self._is_local then
        return require("lib.client_session_payload").build_many(Agent.all_info())
    end
    error("Hub:agent_list is local-only; remote clients receive session entities")
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================

function Hub._before_reload()
    log.info("hub.lua reloading (persistent metatable -- instances auto-upgrade)")
end

function Hub._after_reload()
    local count = 0
    for _ in pairs(remote_hubs) do count = count + 1 end
    log.info(string.format("hub.lua reloaded -- %d remote hubs registered", count))
end

return Hub
