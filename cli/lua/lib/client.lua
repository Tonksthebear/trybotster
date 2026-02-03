-- Client class for managing a single browser connection
--
-- Each Client instance tracks:
-- - Subscriptions (HubChannel, TerminalRelayChannel, etc.)
-- - PTY forwarders for terminal streaming
-- - Connection metadata (peer_id, connected_at)
--
-- This module is hot-reloadable; state is persisted via core.state.

local state = require("core.state")

local Client = {}
Client.__index = Client

--- Create a new Client instance for a browser peer.
-- @param peer_id The unique identifier of the browser peer
-- @return Client instance
function Client.new(peer_id)
    local self = setmetatable({
        peer_id = peer_id,
        subscriptions = {},
        forwarders = {},
        connected_at = os.time(),
    }, Client)

    log.info(string.format("Client created: %s...", peer_id:sub(1, 8)))
    return self
end

--- Route incoming message to appropriate handler.
-- @param msg The decoded JSON message table
function Client:on_message(msg)
    local msg_type = msg.type

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
-- Mirrors Rust's handle_webrtc_subscribe behavior.
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
    webrtc.send(self.peer_id, {
        type = "subscribed",
        subscriptionId = sub_id,
    })

    -- Channel-specific setup
    if channel == "TerminalRelayChannel" then
        self:setup_terminal_subscription(sub_id, agent_index, pty_index)
    elseif channel == "HubChannel" then
        -- Send initial agent and worktree lists
        self:send_agent_list(sub_id)
        self:send_worktree_list(sub_id)
    elseif channel == "PreviewChannel" then
        log.debug(string.format("PreviewChannel subscription: %s", sub_id:sub(1, 16)))
    end
end

--- Set up terminal subscription with PTY forwarder.
-- Creates a forwarder that streams PTY output to the browser.
-- @param sub_id The subscription ID
-- @param agent_index The agent index
-- @param pty_index The PTY index (0=CLI, 1=Server)
function Client:setup_terminal_subscription(sub_id, agent_index, pty_index)
    if agent_index == nil or pty_index == nil then
        log.warn("Terminal subscription missing agent_index or pty_index")
        return
    end

    -- Create PTY forwarder (Rust handles the actual streaming)
    local forwarder = webrtc.create_pty_forwarder({
        peer_id = self.peer_id,
        agent_index = agent_index,
        pty_index = pty_index,
        prefix = "\x01",  -- Binary prefix for raw terminal data
    })

    self.forwarders[sub_id] = forwarder

    -- Request scrollback buffer
    -- Hub will process this and send scrollback via the forwarder
    -- Note: scrollback_key is not currently used; the hub handles retrieving
    -- scrollback data asynchronously and sends it via the forwarder
    hub.get_scrollback(agent_index, pty_index)

    log.info(string.format("Terminal subscription %s: agent=%d, pty=%d",
        sub_id:sub(1, 16), agent_index, pty_index))
end

--- Send agent list to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_agent_list(sub_id)
    local agents = hub.get_agents()
    webrtc.send(self.peer_id, {
        subscriptionId = sub_id,
        command = "list_agents",
        agents = agents,
    })
end

--- Send worktree list to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_worktree_list(sub_id)
    local worktrees = hub.get_worktrees()
    webrtc.send(self.peer_id, {
        subscriptionId = sub_id,
        command = "list_worktrees",
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
        log.debug(string.format("Data for unknown subscription: %s", sub_id:sub(1, 16)))
        return
    end

    -- Determine command source (protocol difference between encrypted/plaintext flows):
    -- - Encrypted flow: command fields at top level (type, data, etc.)
    -- - Plaintext flow: command nested under "data" field
    -- This dual extraction handles both protocol formats transparently.
    local command = msg
    if msg.data and type(msg.data) == "table" then
        command = msg.data
    end

    if sub.channel == "TerminalRelayChannel" then
        self:handle_terminal_data(sub, command)
    elseif sub.channel == "HubChannel" then
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

    if cmd_type == "input" or command.command == "input" then
        -- Forward keyboard input to PTY
        local data = command.data
        if data then
            hub.write_pty(agent_index, pty_index, data)
        end
    elseif cmd_type == "resize" or command.command == "resize" then
        -- Resize PTY
        local rows = command.rows or 24
        local cols = command.cols or 80
        hub.resize_pty(agent_index, pty_index, rows, cols)
    elseif cmd_type == "handshake" or command.command == "handshake" then
        -- Terminal handshake - may include initial size
        log.debug(string.format("Terminal handshake: rows=%s, cols=%s",
            tostring(command.rows), tostring(command.cols)))
        if command.rows and command.cols then
            hub.resize_pty(agent_index, pty_index, command.rows, command.cols)
        end
    else
        log.debug(string.format("Unknown terminal command: %s", tostring(cmd_type)))
    end
end

--- Handle hub control data (list_agents, create_agent, etc.).
-- @param sub_id The subscription ID for responses
-- @param command The hub command
function Client:handle_hub_data(sub_id, command)
    -- Field name inconsistency: command type may be in "type" or "command" field.
    -- TODO: Consolidate on single field name for cleaner future refactoring.
    local cmd_type = command.type or command.command

    if cmd_type == "list_agents" then
        self:send_agent_list(sub_id)

    elseif cmd_type == "list_worktrees" then
        self:send_worktree_list(sub_id)

    elseif cmd_type == "create_agent" then
        local issue_or_branch = command.issue_or_branch or command.branch
        local prompt = command.prompt
        local from_worktree = command.from_worktree

        if issue_or_branch then
            hub.create_agent({
                issue_or_branch = issue_or_branch,
                prompt = prompt,
                from_worktree = from_worktree,
            })
            -- Response will come via agent_created event
            log.info(string.format("Create agent request: %s", issue_or_branch))
        else
            log.warn("create_agent missing issue_or_branch")
        end

    elseif cmd_type == "reopen_worktree" then
        local path = command.path
        local branch = command.branch or ""
        local prompt = command.prompt

        if path then
            hub.create_agent({
                issue_or_branch = branch,
                prompt = prompt,
                from_worktree = path,
            })
            log.info(string.format("Reopen worktree request: %s", path))
        else
            log.warn("reopen_worktree missing path")
        end

    elseif cmd_type == "delete_agent" then
        -- Field name inconsistency: agent ID may be in "id", "agent_id", or "session_key".
        -- TODO: Consolidate on single field name for cleaner future refactoring.
        local agent_id = command.id or command.agent_id or command.session_key
        local delete_worktree = command.delete_worktree or false

        if agent_id then
            hub.delete_agent(agent_id, delete_worktree)
            -- Response will come via agent_deleted event
            log.info(string.format("Delete agent request: %s", agent_id))
        else
            log.warn("delete_agent missing agent_id")
        end

    elseif cmd_type == "select_agent" then
        -- Agent selection for UI state
        log.debug(string.format("Select agent: %s", tostring(command.id or command.agent_index)))

    elseif cmd_type == "handshake" then
        -- Hub handshake - send ack
        local device_name = command.device_name or "unknown"
        log.info(string.format("Hub handshake from %s...: device=%s",
            self.peer_id:sub(1, 8), device_name))

        -- Send ack to complete handshake
        local timestamp = os.time() * 1000  -- milliseconds
        webrtc.send(self.peer_id, {
            subscriptionId = sub_id,
            type = "ack",
            timestamp = timestamp,
        })

    elseif cmd_type == "ack" then
        -- Browser acknowledged our message - nothing to do
        log.debug(string.format("Received ack from %s...", self.peer_id:sub(1, 8)))

    else
        log.debug(string.format("Unknown hub command: %s", tostring(cmd_type)))
    end
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
    log.debug("client.lua reloading...")
end

function Client._after_reload()
    log.debug("client.lua reloaded")
end

return Client
