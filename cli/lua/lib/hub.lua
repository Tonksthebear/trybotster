-- Hub proxy class for transparent local/remote hub access.
--
-- Hub.get(hub_id) returns either a local hub object (direct Lua calls) or a
-- transparent remote proxy (routes through hub_client.request). Plugin
-- authors call Hub.get(params.hub_id):get_pty_snapshot(agent_id, session)
-- without caring whether the hub is local or remote.
--
-- Remote hubs are registered/unregistered by the orchestrator plugin when
-- connections are established or dropped.
--
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable -- existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local Agent = require("lib.agent")

local Hub = state.class("Hub")

-- Remote hub registry (persistent across reloads): hub_id -> conn_id
local remote_hubs = state.get("hub_remote_registry", {})

-- Local hub ID (cached on first access)
local self_id = hub.hub_id()

-- =============================================================================
-- Envelope Helpers
-- =============================================================================

--- Generate a unique message ID.
-- Format: msg_<timestamp>_<random hex> — good enough for session-scoped IDs.
local function generate_msg_id()
    return string.format("msg_%d_%06x", os.time(), math.random(0, 0xffffff))
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

--- Unregister a remote hub connection.
-- Called by the orchestrator plugin on disconnect.
-- @param hub_id string Remote hub identifier
function Hub.unregister(hub_id)
    remote_hubs[hub_id] = nil
    log.info(string.format("Hub.unregister: %s", hub_id))
end

-- =============================================================================
-- Public API — Hub.get()
-- =============================================================================

--- Get a Hub object by ID.
-- Returns a local hub if hub_id is nil or matches self, otherwise a remote proxy.
-- @param hub_id string|nil Hub identifier (nil = local)
-- @return Hub instance
function Hub.get(hub_id)
    -- nil or self -> local
    if not hub_id or hub_id == self_id then
        return new_hub(self_id, true, nil)
    end

    -- Known remote hub
    local conn_id = remote_hubs[hub_id]
    if conn_id then
        return new_hub(hub_id, false, conn_id)
    end

    error(string.format("Hub.get: unknown hub '%s' (not local, not connected)", hub_id))
end

--- Check if a hub ID refers to the local hub.
-- @param hub_id string|nil Hub identifier
-- @return boolean
function Hub.is_local(hub_id)
    return not hub_id or hub_id == self_id
end

-- =============================================================================
-- Instance Methods
-- =============================================================================

--- Get a PTY snapshot from an agent session.
-- Local: calls Agent directly. Remote: uses hub_client.request().
-- @param agent_id string Agent key
-- @param session string|nil Session name (default "agent")
-- @return string Snapshot content
function Hub:get_pty_snapshot(agent_id, session)
    session = session or "agent"

    if self._is_local then
        local agent = Agent.get(agent_id)
        if not agent then
            error(string.format("Hub:get_pty_snapshot: agent '%s' not found", agent_id))
        end
        local handle = agent.sessions[session]
        if not handle then
            error(string.format("Hub:get_pty_snapshot: session '%s' not found on agent '%s'",
                session, agent_id))
        end
        return handle:get_screen()
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
-- @param agent_id string Agent key
-- @param text string Message text to deliver
-- @param session string|nil Session name (default "agent")
function Hub:send_message(agent_id, text, session)
    session = session or "agent"

    if self._is_local then
        local agent = Agent.get(agent_id)
        if not agent then
            error(string.format("Hub:send_message: agent '%s' not found", agent_id))
        end
        local handle = agent.sessions[session]
        if not handle then
            error(string.format("Hub:send_message: session '%s' not found on agent '%s'",
                session, agent_id))
        end
        handle:send_message(text)
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
-- @param agent_id string Agent key
-- @param opts table { type, payload, reply_to, expires_in, session, from_agent_id }
-- @return table { msg_id, status }
function Hub:post(agent_id, opts)
    opts = opts or {}
    local msg_type = opts.type or "message"
    local session = opts.session or "agent"

    if self._is_local then
        local agent = Agent.get(agent_id)
        if not agent then
            error(string.format("Hub:post: agent '%s' not found", agent_id))
        end

        if msg_type == "notify" then
            -- PTY-only: write text directly, no inbox, no doorbell
            local handle = agent.sessions[session]
            if not handle then
                error(string.format("Hub:post: session '%s' not found on agent '%s'",
                    session, agent_id))
            end
            handle:send_message(opts.payload or "")
            return { msg_id = nil, status = "delivered" }
        end

        -- Build envelope — hub injects msg_id and timestamps
        local envelope = build_envelope(self.id, opts.from_agent_id or "unknown", opts)

        -- Write to inbox directly
        agent._inbox = agent._inbox or {}
        table.insert(agent._inbox, envelope)

        -- PTY doorbell — minimal trigger line only, payload stays in inbox
        local handle = agent.sessions[session]
        if handle then
            handle:send_message(string.format(
                "\n\xe2\xac\xa1 [botster-mcp] new message from %s \xe2\x80\x94 use receive_messages() via botster MCP\n",
                envelope.from.agent_id
            ))
            return { msg_id = envelope.msg_id, status = "delivered" }
        end

        -- Inbox written but session was missing — message is readable via receive_messages()
        -- but agent won't see a doorbell
        log.warn(string.format("Hub:post: inbox written for %s but session '%s' not found, no doorbell",
            agent_id, session))
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
        session       = session,
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
-- @param agent_id string Agent key
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

--- Create an agent on this hub.
-- Local: calls handlers.agents directly. Remote: uses hub_client.request().
-- @param issue_or_branch string Issue number or branch name
-- @param prompt string|nil Task prompt
-- @param profile string|nil Config profile name
-- @return string Result message
function Hub:create_agent(issue_or_branch, prompt, profile)
    if self._is_local then
        local agents_handler = require("handlers.agents")
        local agent = agents_handler.handle_create_agent(
            issue_or_branch, prompt, nil, nil, profile
        )
        if agent then
            return "Agent created: " .. agent:agent_key()
        else
            return "Agent creation initiated (worktree may be creating async)"
        end
    end

    local result = hub_client.request(self._conn_id, {
        type = "create_agent",
        issue_or_branch = issue_or_branch,
        prompt = prompt,
        profile = profile,
    }, 60000)

    if result.error then
        error(string.format("Hub:create_agent remote error: %s", result.error))
    end

    return result.result
end

--- Delete an agent on this hub.
-- Local: calls handlers.agents directly. Remote: uses hub_client.request().
-- @param agent_id string Agent key
-- @param delete_worktree boolean|nil Also delete the git worktree (default false)
-- @return string Result message
function Hub:delete_agent(agent_id, delete_worktree)
    if self._is_local then
        local agents_handler = require("handlers.agents")
        local deleted = agents_handler.handle_delete_agent(agent_id, delete_worktree or false)
        if deleted then
            return "Agent deleted: " .. agent_id
        else
            return "Agent not found: " .. agent_id
        end
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
