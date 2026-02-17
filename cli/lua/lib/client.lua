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
-- This module is hot-reloadable; state is persisted via hub.state.
-- Uses state.class() for persistent metatable — existing instances
-- automatically see new/changed methods after hot-reload.

local state = require("hub.state")
local Agent = require("lib.agent")
local pty_clients = require("lib.pty_clients")

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

    -- Interceptor: plugins can transform or block subscriptions (return nil)
    local result = hooks.call("before_client_subscribe", {
        client = self,
        sub_id = sub_id,
        channel = channel,
        params = params,
    })
    if result == nil then
        log.info(string.format("before_client_subscribe interceptor blocked: %s", sub_id:sub(1, 16)))
        return
    end
    -- Allow interceptors to modify fields
    channel = result.channel or channel
    params = result.params or params

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

    hooks.notify("client_subscribed", {
        peer_id = self.peer_id,
        channel = channel,
        sub_id = sub_id,
        params = params,
    })

    hooks.notify("after_client_subscribe", {
        client = self,
        sub_id = sub_id,
        channel = channel,
    })

    -- Channel-specific setup
    if channel == "terminal" then
        -- Register with pty_clients for dimension tracking.
        -- This resizes the PTY before the forwarder is created.
        if agent_index ~= nil and pty_index ~= nil then
            pty_clients.register(agent_index, pty_index, self.peer_id,
                params.rows or 24, params.cols or 80)
        end
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
-- Both TUI and WebRTC use the same code path: pty_clients handles resize,
-- transport.create_pty_forwarder handles output streaming.
--
-- @param sub_id The subscription ID
-- @param agent_index The agent index
-- @param pty_index The PTY index (0=CLI, 1=Server)
function Client:setup_terminal_subscription(sub_id, agent_index, pty_index)
    if agent_index == nil or pty_index == nil then
        log.warn("Terminal subscription missing agent_index or pty_index")
        return
    end

    -- PTY was already resized by pty_clients.register() in handle_subscribe().
    -- Now create the forwarder to stream output to this client.
    local rows, cols = pty_clients.get_active_dims(agent_index, pty_index)
    rows = rows or 24
    cols = cols or 80

    local forwarder = self.transport.create_pty_forwarder({
        agent_index = agent_index,
        pty_index = pty_index,
        subscription_id = sub_id,
        prefix = "\x01",  -- Binary prefix for raw terminal data
    })

    self.forwarders[sub_id] = forwarder

    log.info(string.format("Terminal subscription %s: agent=%d, pty=%d (%dx%d)",
        sub_id:sub(1, 16), agent_index, pty_index, cols, rows))
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

    -- Unregister from pty_clients (auto-resizes to next client if any)
    if sub.channel == "terminal" and sub.agent_index ~= nil and sub.pty_index ~= nil then
        pty_clients.unregister(sub.agent_index, sub.pty_index, self.peer_id)
    end

    hooks.notify("client_unsubscribed", {
        peer_id = self.peer_id,
        channel = sub.channel,
        sub_id = sub_id,
    })

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

--- Handle terminal control messages (resize).
--- Input is handled via binary CONTENT_PTY frames directly in Rust (poll_pty_input).
-- @param sub The subscription info
-- @param command The terminal command
function Client:handle_terminal_data(sub, command)
    local agent_index = sub.agent_index or 0
    local pty_index = sub.pty_index or 0
    local cmd_type = command.type

    log.debug(string.format("handle_terminal_data: cmd_type=%s, agent=%d, pty=%d",
        tostring(cmd_type), agent_index, pty_index))

    if cmd_type == "resize" or command.command == "resize" then
        local rows = command.rows or 24
        local cols = command.cols or 80
        pty_clients.update(agent_index, pty_index, self.peer_id, rows, cols)
    else
        log.debug(string.format("Unknown terminal command: %s", tostring(cmd_type)))
    end
end

--- Handle hub control data (list_agents, create_agent, etc.).
-- Runs the before_hub_command interceptor chain, then dispatches
-- to the command registry. See lib/commands.lua and handlers/commands.lua.
-- @param sub_id The subscription ID for responses
-- @param command The hub command
function Client:handle_hub_data(sub_id, command)
    local cmd_type = command.type or command.command
    log.debug(string.format("handle_hub_data: type=%s", tostring(cmd_type)))

    -- Interceptor chain: transform, validate, or drop before dispatch.
    -- Return nil from an interceptor to block the command entirely.
    -- Only pass command through the chain (self/sub_id are context, not transformable).
    command = hooks.call("before_hub_command", command)
    if command == nil then return end

    require("lib.commands").dispatch(self, sub_id, command)
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
-- Stops all forwarders, unregisters from pty_clients, and clears subscriptions.
function Client:disconnect()
    hooks.notify("before_client_disconnect", { peer_id = self.peer_id })

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

    -- Unregister from all terminal PTYs (auto-resizes to next client)
    for _, sub in pairs(self.subscriptions) do
        if sub.channel == "terminal" and sub.agent_index ~= nil and sub.pty_index ~= nil then
            pty_clients.unregister(sub.agent_index, sub.pty_index, self.peer_id)
        end
    end
    self.subscriptions = {}

    local duration = os.time() - self.connected_at
    log.info(string.format("Client disconnected: %s... (session: %ds)",
        self.peer_id:sub(1, 8), duration))
end

-- Lifecycle hooks for hot-reload
function Client._before_reload()
    log.info("client.lua reloading (persistent metatable — instances auto-upgrade)")
end

function Client._after_reload()
    log.info("client.lua reloaded — all existing instances now use new methods")
end

return Client
