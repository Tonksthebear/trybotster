-- Client class for managing a single peer connection
--
-- Each Client instance tracks:
-- - Subscriptions (HubChannel, TerminalRelayChannel, etc.)
-- - PTY forwarders for terminal streaming
-- - Connection metadata (peer_id, connected_at)
-- - Transport for sending messages back to the peer
--
-- Transport-agnostic: works with any transport that provides send(msg)
-- and send_binary(data) methods. Currently supports WebRTC and TUI transports.
--
-- This module is hot-reloadable; state is persisted via core.state.
-- Uses state.class() for persistent metatable — existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("core.state")
local Agent = require("lib.agent")

-- Lazy-load handlers.agents to avoid circular dependency at require-time.
-- client.lua is a lib module loaded before handlers, so handlers.agents
-- may not exist yet when this file first loads. The accessor defers the
-- require() to first use, which is always after init.lua finishes loading.
local _agents_handler
local function get_agents_handler()
    if not _agents_handler then
        _agents_handler = require("handlers.agents")
    end
    return _agents_handler
end

local Client = state.class("client")

--- Create a new Client instance for a peer connection.
-- @param peer_id The unique identifier of the peer
-- @param transport Table with send(msg) and send_binary(data) methods
-- @return Client instance
function Client.new(peer_id, transport)
    assert(transport, "Client.new requires a transport")
    assert(transport.send, "transport must have a send(msg) method")

    local self = setmetatable({
        peer_id = peer_id,
        transport = transport,
        subscriptions = {},
        forwarders = {},
        connected_at = os.time(),
        -- Terminal dimensions (updated via hub channel resize messages)
        rows = 24,
        cols = 80,
    }, Client)

    log.info(string.format("Client created: %s...", peer_id:sub(1, 8)))
    return self
end

--- Send a structured message to the peer.
-- @param msg The message table to send
function Client:send(msg)
    self.transport.send(msg)
end

--- Send binary data to the peer.
-- @param data The binary data to send
function Client:send_binary(data)
    if self.transport.send_binary then
        self.transport.send_binary(data)
    else
        log.warn(string.format("Client %s... transport has no send_binary", self.peer_id:sub(1, 8)))
    end
end

--- Update client terminal dimensions and resize active PTY forwarders.
-- Called when browser sends resize via hub channel.
-- @param rows Number of rows
-- @param cols Number of columns
function Client:update_dims(rows, cols)
    local old_rows, old_cols = self.rows, self.cols
    self.rows = rows
    self.cols = cols

    if old_rows == rows and old_cols == cols then
        return  -- No change
    end

    log.debug(string.format("Client %s... dims updated: %dx%d -> %dx%d",
        self.peer_id:sub(1, 8), old_cols, old_rows, cols, rows))

    -- Resize all active PTY forwarders for this client
    for sub_id, sub in pairs(self.subscriptions) do
        if sub.channel == "terminal" and sub.agent_index ~= nil and sub.pty_index ~= nil then
            hub.resize_pty(sub.agent_index, sub.pty_index, rows, cols)
            log.debug(string.format("Resized PTY for subscription %s: %dx%d",
                sub_id:sub(1, 16), cols, rows))
        end
    end
end

--- Route incoming message to appropriate handler.
-- @param msg The decoded JSON message table
function Client:on_message(msg)
    local msg_type = msg.type
    log.debug(string.format("on_message: type=%s, subId=%s",
        tostring(msg_type), tostring(msg.subscriptionId and msg.subscriptionId:sub(1,16) or "nil")))

    if msg_type == "subscribe" then
        self:handle_subscribe(msg)
        return
    elseif msg_type == "unsubscribe" then
        self:handle_unsubscribe(msg)
        return
    end

    -- Data messages have subscriptionId but no subscribe/unsubscribe type
    if msg.subscriptionId then
        self:handle_data(msg)
    else
        log.debug(string.format("Unknown message from %s...: type=%s",
            self.peer_id:sub(1, 8), tostring(msg_type)))
    end
end

--- Handle subscribe message - create virtual subscription.
-- Handles WebRTC subscribe requests for PTY output streaming.
-- @param msg The subscribe message
function Client:handle_subscribe(msg)
    local sub_id = msg.subscriptionId
    if not sub_id then
        log.error("Subscribe message missing subscriptionId")
        return
    end

    local channel = msg.channel or "unknown"
    local params = msg.params or {}
    local agent_index = params.agent_index
    local pty_index = params.pty_index

    log.debug(string.format("Subscribe: %s -> %s (agent=%s, pty=%s)",
        sub_id:sub(1, 16), channel,
        tostring(agent_index), tostring(pty_index)))

    -- Store subscription info
    self.subscriptions[sub_id] = {
        channel = channel,
        agent_index = agent_index,
        pty_index = pty_index,
    }

    -- Send subscription confirmation immediately
    -- Browser waits for this before allowing input
    self:send({
        type = "subscribed",
        subscriptionId = sub_id,
    })

    -- Channel-specific setup
    if channel == "terminal" then
        self:setup_terminal_subscription(sub_id, agent_index, pty_index)
    elseif channel == "hub" then
        -- Send initial agent and worktree lists
        log.info(string.format("Hub subscription from %s...", self.peer_id:sub(1, 8)))
        self:send_agent_list(sub_id)
        self:send_worktree_list(sub_id)
    elseif channel == "preview" then
        log.debug(string.format("Preview subscription: %s", sub_id:sub(1, 16)))
    end
end

--- Set up terminal subscription with PTY forwarder.
-- Creates a transport-agnostic forwarder that streams PTY output to the client.
--
-- For TUI clients, uses direct session access (tui.forward_session) which
-- bypasses HandleCache. For WebRTC clients, uses index-based lookup.
--
-- @param sub_id The subscription ID (browser-generated, e.g., "sub_2_1770164017")
-- @param agent_index The agent index
-- @param pty_index The PTY index (0=CLI, 1=Server)
function Client:setup_terminal_subscription(sub_id, agent_index, pty_index)
    if agent_index == nil or pty_index == nil then
        log.warn("Terminal subscription missing agent_index or pty_index")
        return
    end

    -- Map pty_index to session name
    local session_name = pty_index == 0 and "cli" or "server"

    -- Get agent from Lua registry by index
    local agents = Agent.list()
    local agent = agents[agent_index + 1]  -- Lua 1-indexed

    if agent and self.transport.type == "tui" then
        -- TUI: Use direct session access (no HandleCache needed)
        local session = agent.sessions[session_name]
        if session then
            local forwarder = tui.forward_session({
                agent_key = agent:agent_key(),
                session_name = session_name,
                session = session,
                subscription_id = sub_id,
            })
            self.forwarders[sub_id] = forwarder

            -- Resize PTY to client's current dimensions using direct session method
            session:resize(self.rows, self.cols)

            log.info(string.format("Terminal subscription %s: %s:%s (%dx%d) [direct]",
                sub_id:sub(1, 16), agent:agent_key(), session_name, self.cols, self.rows))
            return
        else
            log.warn(string.format("No session '%s' on agent %s", session_name, agent:agent_key()))
        end
    end

    -- Fallback: Use index-based lookup (for WebRTC or if agent not found)
    local forwarder = self.transport.create_pty_forwarder({
        agent_index = agent_index,
        pty_index = pty_index,
        subscription_id = sub_id,
        prefix = "\x01",  -- Binary prefix for raw terminal data
    })

    self.forwarders[sub_id] = forwarder

    -- Resize PTY to client's current dimensions
    hub.resize_pty(agent_index, pty_index, self.rows, self.cols)

    -- Request scrollback buffer
    hub.get_scrollback(agent_index, pty_index)

    log.info(string.format("Terminal subscription %s: agent=%d, pty=%d (%dx%d)",
        sub_id:sub(1, 16), agent_index, pty_index, self.cols, self.rows))
end

--- Send agent list to a HubChannel subscription.
-- Uses Agent registry for rich metadata (repo, issue, branch, status, etc.).
-- @param sub_id The subscription ID to send to
function Client:send_agent_list(sub_id)
    self:send({
        subscriptionId = sub_id,
        type = "agent_list",
        agents = Agent.all_info(),
    })
end

--- Send worktree list to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_worktree_list(sub_id)
    local worktrees = hub.get_worktrees()
    log.info(string.format("Sending worktree list: %d worktrees", #worktrees))
    for i, wt in ipairs(worktrees) do
        log.debug(string.format("  Worktree %d: %s (%s)", i, wt.path or "?", wt.branch or "?"))
    end
    self:send({
        subscriptionId = sub_id,
        type = "worktree_list",
        worktrees = worktrees,
    })
end

--- Handle unsubscribe message - remove virtual subscription.
-- @param msg The unsubscribe message
function Client:handle_unsubscribe(msg)
    local sub_id = msg.subscriptionId
    if not sub_id then
        log.error("Unsubscribe message missing subscriptionId")
        return
    end

    local sub = self.subscriptions[sub_id]
    if not sub then
        log.debug(string.format("Unsubscribe for unknown subscription: %s", sub_id:sub(1, 16)))
        return
    end

    -- Stop forwarder if this was a terminal subscription
    local forwarder = self.forwarders[sub_id]
    if forwarder then
        forwarder:stop()
        self.forwarders[sub_id] = nil
        log.debug(string.format("Stopped forwarder for subscription: %s", sub_id:sub(1, 16)))
    end

    self.subscriptions[sub_id] = nil
    log.info(string.format("Unsubscribed: %s (was %s)", sub_id:sub(1, 16), sub.channel))
end

--- Handle data message for an existing subscription.
-- Routes to terminal or hub data handlers based on channel.
-- @param msg The data message
function Client:handle_data(msg)
    local sub_id = msg.subscriptionId
    local sub = self.subscriptions[sub_id]

    if not sub then
        log.warn(string.format("Data for unknown subscription: %s (known subs: %d)",
            sub_id:sub(1, 16), self:count_subscriptions()))
        return
    end

    log.debug(string.format("handle_data: subId=%s, channel=%s, type=%s",
        sub_id:sub(1, 16), sub.channel, tostring(msg.type or msg.data and msg.data.type)))

    -- Determine command source (protocol difference between encrypted/plaintext flows):
    -- - Encrypted flow: command fields at top level (type, data, etc.)
    -- - Plaintext flow: command nested under "data" field
    -- This dual extraction handles both protocol formats transparently.
    local command = msg
    if msg.data and type(msg.data) == "table" then
        command = msg.data
    end

    if sub.channel == "terminal" then
        self:handle_terminal_data(sub, command)
    elseif sub.channel == "hub" then
        self:handle_hub_data(sub_id, command)
    end
end

--- Handle terminal data (input, resize, handshake).
-- @param sub The subscription info
-- @param command The terminal command
function Client:handle_terminal_data(sub, command)
    local agent_index = sub.agent_index or 0
    local pty_index = sub.pty_index or 0
    local cmd_type = command.type

    log.debug(string.format("handle_terminal_data: cmd_type=%s, agent=%d, pty=%d",
        tostring(cmd_type), agent_index, pty_index))

    if cmd_type == "input" or command.command == "input" then
        -- Forward keyboard input to PTY
        local data = command.data
        if data then
            log.debug(string.format("PTY input: agent=%d, pty=%d, len=%d",
                agent_index, pty_index, #data))
            hub.write_pty(agent_index, pty_index, data)
        else
            log.warn("PTY input: data is nil!")
        end
    elseif cmd_type == "resize" or command.command == "resize" then
        -- Resize PTY and update client dims
        local rows = command.rows or 24
        local cols = command.cols or 80
        self.rows = rows
        self.cols = cols
        hub.resize_pty(agent_index, pty_index, rows, cols)
    else
        log.debug(string.format("Unknown terminal command: %s", tostring(cmd_type)))
    end
end

--- Handle hub control data (list_agents, create_agent, etc.).
-- @param sub_id The subscription ID for responses
-- @param command The hub command
function Client:handle_hub_data(sub_id, command)
    -- Field name inconsistency: command type may be in "type" or "command" field.
    local cmd_type = command.type or command.command
    log.debug(string.format("handle_hub_data: type=%s", tostring(cmd_type)))

    if cmd_type == "list_agents" then
        self:send_agent_list(sub_id)

    elseif cmd_type == "list_worktrees" then
        self:send_worktree_list(sub_id)

    elseif cmd_type == "create_agent" then
        local issue_or_branch = command.issue_or_branch or command.branch
        local prompt = command.prompt
        local from_worktree = command.from_worktree

        get_agents_handler().handle_create_agent(issue_or_branch, prompt, from_worktree, self)
        log.info(string.format("Create agent request: %s", tostring(issue_or_branch or "main")))

    elseif cmd_type == "reopen_worktree" then
        local path = command.path
        local branch = command.branch or ""
        local prompt = command.prompt

        if path then
            get_agents_handler().handle_create_agent(branch, prompt, path, self)
            log.info(string.format("Reopen worktree request: %s", path))
        else
            log.warn("reopen_worktree missing path")
        end

    elseif cmd_type == "delete_agent" then
        -- Field name inconsistency: agent ID may be in "id", "agent_id", or "session_key".
        local agent_id = command.id or command.agent_id or command.session_key
        local delete_worktree = command.delete_worktree or false

        if agent_id then
            get_agents_handler().handle_delete_agent(agent_id, delete_worktree)
            log.info(string.format("Delete agent request: %s", agent_id))
        else
            log.warn("delete_agent missing agent_id")
        end

    elseif cmd_type == "select_agent" then
        -- No backend action needed; agent selection is client-side UI state
        log.debug(string.format("Select agent: %s", tostring(command.id or command.agent_index)))

    elseif cmd_type == "get_connection_code" then
        -- Always go through Hub-side generation which includes QR PNG.
        -- generate_connection_url() is idempotent (returns cached bundle
        -- unless consumed by a browser, in which case it auto-regenerates).
        connection.generate()

    elseif cmd_type == "regenerate_connection_code" then
        -- Force-regenerate: creates a fresh PreKeyBundle unconditionally
        connection.regenerate()
        log.info("Connection code regeneration requested")

    elseif cmd_type == "resize" then
        -- Client resize - update stored dims and resize any active PTY forwarders
        local rows = command.rows or 24
        local cols = command.cols or 80
        self:update_dims(rows, cols)

    elseif cmd_type == "quit" then
        hub.quit()

    elseif cmd_type == "copy_connection_url" then
        connection.copy_to_clipboard()

    else
        log.debug(string.format("Unknown hub command: %s", tostring(cmd_type)))
    end
end

--- Count active subscriptions (for debugging).
-- @return Number of subscriptions
function Client:count_subscriptions()
    local count = 0
    for _ in pairs(self.subscriptions) do
        count = count + 1
    end
    return count
end

--- Clean up client on disconnect.
-- Stops all forwarders and clears subscriptions.
function Client:disconnect()
    -- Stop all forwarders with error protection to prevent early exit
    for sub_id, forwarder in pairs(self.forwarders) do
        if forwarder and forwarder.stop then
            local ok, err = pcall(forwarder.stop, forwarder)
            if not ok then
                log.warn(string.format("Error stopping forwarder %s: %s", sub_id, tostring(err)))
            end
        end
    end
    self.forwarders = {}
    self.subscriptions = {}

    local duration = os.time() - self.connected_at
    log.info(string.format("Client disconnected: %s... (session: %ds)",
        self.peer_id:sub(1, 8), duration))
end

-- Lifecycle hooks for hot-reload
function Client._before_reload()
    -- Clear cached handler reference so next call picks up the fresh module.
    -- Handles the case where handlers.agents was reloaded independently.
    _agents_handler = nil
    log.info("client.lua reloading (persistent metatable — instances auto-upgrade)")
end

function Client._after_reload()
    log.info("client.lua reloaded — all existing instances now use new methods")
end

return Client
