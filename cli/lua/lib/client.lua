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
    elseif msg_type == "hello" then
        self:handle_hello(msg)
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

--- Handle socket protocol hello message.
-- This is advisory negotiation: we ack our protocol version so newer clients
-- can detect capabilities, but we do not gate message handling on this.
-- @param msg The hello message
function Client:handle_hello(msg)
    local peer_version = tonumber(msg.protocol_version) or 1
    self.socket_protocol_version = peer_version

    self:send({
        type = "hello_ack",
        protocol_version = 2,
        min_supported_version = 1,
        features = {
            scrollback_dims = true,
            process_exited = true,
        },
    })

    log.debug(string.format(
        "Socket protocol hello from %s... (peer_version=%d)",
        self.peer_id:sub(1, 8),
        peer_version))
end

--- Handle subscribe message - create virtual subscription.
-- @param msg The subscribe message
function Client:handle_subscribe(msg)
    local sub_id = msg.subscriptionId
    if not sub_id then
        log.error("Subscribe message missing subscriptionId")
        return
    end

    local channel = msg.channel or "unknown"
    local params = msg.params or {}
    local session_uuid = params.session_uuid

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

    session_uuid = params.session_uuid

    -- Idempotency: a reconnect race can deliver duplicate subscribe messages
    -- for the same subscription ID. Treat exact duplicates as no-op setup to
    -- avoid creating two forwarders and replaying scrollback twice.
    local existing = self.subscriptions[sub_id]
    if existing then
        if existing.channel == channel and existing.session_uuid == session_uuid then
            local rows = params.rows or 24
            local cols = params.cols or 80
            local recreated = false
            if channel == "terminal" and session_uuid then
                local existing_forwarder = self.forwarders[sub_id]
                if (not existing_forwarder) or (not existing_forwarder:is_active()) then
                    if existing_forwarder then
                        existing_forwarder:stop()
                    end
                    self:setup_terminal_subscription(sub_id, session_uuid, rows, cols)
                    recreated = true
                end
                pty_clients.update(session_uuid, self.peer_id, rows, cols)
            end

            if recreated then
                log.info(string.format(
                    "Duplicate subscribe recreated stale forwarder: %s (peer=%s)",
                    sub_id:sub(1, 16), self.peer_id:sub(1, 8)))
            else
                log.debug(string.format(
                    "Duplicate subscribe no-op: %s -> %s (peer=%s)",
                    sub_id:sub(1, 16), channel, self.peer_id:sub(1, 8)))
            end

            self:send({
                type = "subscribed",
                subscriptionId = sub_id,
            })
            return
        end

        log.warn(string.format(
            "Replacing existing subscription: %s (%s -> %s)",
            sub_id:sub(1, 16), tostring(existing.channel), tostring(channel)))

        local existing_forwarder = self.forwarders[sub_id]
        if existing_forwarder then
            existing_forwarder:stop()
            self.forwarders[sub_id] = nil
        end
        if existing.channel == "terminal" and existing.session_uuid then
            pty_clients.unregister(existing.session_uuid, self.peer_id)
        end
    end

    log.info(string.format("Subscribe: %s -> %s (peer=%s, session=%s, rows=%s, cols=%s)",
        sub_id:sub(1, 16), channel, self.peer_id:sub(1, 8),
        tostring(session_uuid and session_uuid:sub(1, 16) or "nil"),
        tostring(params.rows), tostring(params.cols)))

    -- Store subscription info
    self.subscriptions[sub_id] = {
        channel = channel,
        session_uuid = session_uuid,
        rows = params.rows or 24,
        cols = params.cols or 80,
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
        local rows = params.rows or 24
        local cols = params.cols or 80
        if rows < 2 or cols < 2 then
            log.warn(string.format(
                "Suspicious terminal dimensions from %s: %dx%d (session=%s)",
                self.peer_id:sub(1, 8), cols, rows,
                tostring(session_uuid and session_uuid:sub(1, 16) or "nil")))
        end

        if session_uuid then
            -- Important ordering:
            -- 1) create forwarder (captures authoritative snapshot)
            -- 2) apply resize intent for this client
            --
            self:setup_terminal_subscription(sub_id, session_uuid, rows, cols)
            pty_clients.register(session_uuid, self.peer_id, rows, cols)
        end
    elseif channel == "hub" then
        -- Send initial agent and worktree lists
        log.info(string.format("Hub subscription from %s...", self.peer_id:sub(1, 8)))
        self:send_agent_list(sub_id)
        self:send_workspace_list(sub_id)
        self:send_spawn_target_list(sub_id)
        self:send_hub_recovery_state(sub_id)
    elseif channel == "mcp" then
        -- MCP is pull-based: the client sends tools/list when ready.
        self.subscriptions[sub_id].caller_context = params.context or {}
        local ctx = params.context or {}
        log.info(string.format("MCP subscription from %s... (agent=%s, hub=%s)",
            self.peer_id:sub(1, 8), tostring(ctx.session_uuid), tostring(ctx.hub_id)))
    elseif channel == "preview" then
        log.debug(string.format("Preview subscription: %s", sub_id:sub(1, 16)))
    end
end

--- Send admitted spawn targets to a HubChannel subscription.
-- Includes live inspection metadata such as git status and current branch.
-- @param sub_id The subscription ID to send to
function Client:send_spawn_target_list(sub_id)
    local targets = {}
    local registry = rawget(_G, "spawn_targets")
    if registry and type(registry.list) == "function" then
        local ok, listed = pcall(registry.list)
        if ok and type(listed) == "table" then
            for _, target in ipairs(listed) do
                local merged = target
                if type(registry.inspect) == "function" and target.path then
                    local inspect_ok, inspection = pcall(registry.inspect, target.path)
                    if inspect_ok and type(inspection) == "table" then
                        merged = {}
                        for k, v in pairs(target) do merged[k] = v end
                        for k, v in pairs(inspection) do merged[k] = v end
                    end
                end
                targets[#targets + 1] = merged
            end
        end
    end

    self:send({
        subscriptionId = sub_id,
        type = "spawn_target_list",
        targets = targets,
    })
end

--- Set up terminal subscription with PTY forwarder.
-- Creates a transport-agnostic forwarder that streams PTY output to the client.
--
-- @param sub_id The subscription ID
-- @param rows number|nil Requested rows from subscriber
-- @param cols number|nil Requested cols from subscriber
function Client:setup_terminal_subscription(sub_id, session_uuid, rows, cols)
    if not session_uuid then
        log.warn("Terminal subscription missing session_uuid")
        return
    end

    rows = rows or 24
    cols = cols or 80

    local forwarder = self.transport.create_pty_forwarder({
        session_uuid = session_uuid,
        subscription_id = sub_id,
        rows = rows,
        cols = cols,
        prefix = "\x01",  -- Binary prefix for raw terminal data
    })

    self.forwarders[sub_id] = forwarder

    log.info(string.format("Terminal subscription %s: session=%s (%dx%d)",
        sub_id:sub(1, 16), session_uuid:sub(1, 16), cols, rows))
end

--- Send agent list to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_agent_list(sub_id)
    local payload = require("lib.agent_list_payload").build(Agent.all_info())
    self:send({
        subscriptionId = sub_id,
        type = "agent_list",
        agents = payload.agents,
        workspaces = payload.workspaces,
    })
end

--- Send workspace list to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_workspace_list(sub_id)
    local Hub = require("lib.hub")
    local ok, workspaces = pcall(function()
        return Hub.get():list_workspaces()
    end)
    if not ok then
        log.warn(string.format("Failed to build workspace list: %s", tostring(workspaces)))
        workspaces = {}
    end
    self:send({
        subscriptionId = sub_id,
        type = "workspace_list",
        workspaces = workspaces,
    })
end

--- Send open workspace list to a HubChannel subscription.
-- Includes only workspaces that currently have running sessions.
-- @param sub_id The subscription ID to send to
function Client:send_open_workspace_list(sub_id)
    local payload = require("lib.agent_list_payload").build(Agent.all_info())
    self:send({
        subscriptionId = sub_id,
        type = "open_workspace_list",
        workspaces = payload.workspaces,
    })
end

--- Send worktree list to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_worktree_list(sub_id, target)
    local worktrees = {}
    local WorktreeListPayload = require("lib.worktree_list_payload")
    local registry = rawget(_G, "spawn_targets")
    local inspection = nil
    if registry and target and target.target_path and type(registry.inspect) == "function" then
        local ok, result = pcall(registry.inspect, target.target_path)
        if ok then
            inspection = result
        end
    end

    if inspection and inspection.supports_worktrees then
        local repo_root = inspection.repo_root or target.target_path
        local ok, listed = pcall(worktree.list_for_root, repo_root)
        if ok and type(listed) == "table" then
            worktrees = listed
        else
            log.warn(string.format(
                "Failed to list worktrees for target %s: %s",
                tostring(target and target.target_id),
                tostring(listed)
            ))
        end
    end
    worktrees = WorktreeListPayload.build(target, worktrees, Agent.all_info())
    log.info(string.format("Sending worktree list: %d worktrees", #worktrees))
    for i, wt in ipairs(worktrees) do
        log.debug(string.format("  Worktree %d: %s (%s)", i, wt.path or "?", wt.branch or "?"))
    end
    self:send({
        subscriptionId = sub_id,
        type = "worktree_list",
        target_id = target and target.target_id or nil,
        target_path = target and target.target_path or nil,
        target_repo = target and target.target_repo or nil,
        worktrees = worktrees,
    })
end

--- Send current hub recovery lifecycle state to a HubChannel subscription.
-- @param sub_id The subscription ID to send to
function Client:send_hub_recovery_state(sub_id)
    local recovery = state.get("connections.hub_recovery_state", { state = "starting" })
    local message = {
        subscriptionId = sub_id,
        type = "hub_recovery_state",
    }
    for k, v in pairs(recovery) do
        message[k] = v
    end
    self:send(message)
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
    if sub.channel == "terminal" and sub.session_uuid then
        pty_clients.unregister(sub.session_uuid, self.peer_id)
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
    local command = msg
    if msg.data and type(msg.data) == "table" then
        command = msg.data
    end

    if sub.channel == "terminal" then
        self:handle_terminal_data(sub_id, sub, command)
    elseif sub.channel == "hub" then
        self:handle_hub_data(sub_id, command)
    elseif sub.channel == "mcp" then
        self:handle_mcp_data(sub_id, command)
    end
end

--- Handle terminal control messages (resize).
--- Input is handled via binary CONTENT_PTY frames directly in Rust (poll_pty_input).
-- @param sub_id The subscription id
-- @param sub The subscription info
-- @param command The terminal command
function Client:handle_terminal_data(sub_id, sub, command)
    local session_uuid = sub.session_uuid
    local cmd_type = command.type

    log.debug(string.format("handle_terminal_data: cmd_type=%s, session=%s",
        tostring(cmd_type), tostring(session_uuid and session_uuid:sub(1, 16) or "nil")))

    if cmd_type == "resize" or command.command == "resize" then
        local rows = command.rows or 24
        local cols = command.cols or 80
        sub.rows = rows
        sub.cols = cols
        if session_uuid then
            log.info(string.format("Resize: peer=%s, session=%s, %dx%d",
                self.peer_id:sub(1, 8), session_uuid:sub(1, 16), cols, rows))
            pty_clients.update(session_uuid, self.peer_id, rows, cols)
        end
    elseif cmd_type == "request_snapshot" then
        if session_uuid and self.transport.request_pty_snapshot then
            local rows = command.rows or sub.rows or 24
            local cols = command.cols or sub.cols or 80
            sub.rows = rows
            sub.cols = cols
            log.info(string.format("Snapshot refresh: peer=%s, session=%s, %dx%d",
                self.peer_id:sub(1, 8), session_uuid:sub(1, 16), cols, rows))
            self.transport.request_pty_snapshot({
                session_uuid = session_uuid,
                subscription_id = sub_id,
                rows = rows,
                cols = cols,
            })
        else
            log.warn(string.format("Snapshot refresh unavailable for %s", tostring(sub_id)))
        end
    else
        log.debug(string.format("Unknown terminal command: %s", tostring(cmd_type)))
    end
end

--- Handle hub control data (list_agents, create_agent, etc.).
-- @param sub_id The subscription ID for responses
-- @param command The hub command
function Client:handle_hub_data(sub_id, command)
    local cmd_type = command.type or command.command
    log.debug(string.format("handle_hub_data: type=%s", tostring(cmd_type)))

    -- Interceptor chain
    command = hooks.call("before_hub_command", command)
    if command == nil then return end

    require("lib.commands").dispatch(self, sub_id, command)
end

--- Handle MCP channel data messages.
-- @param sub_id The subscription ID
-- @param command The command message
function Client:handle_mcp_data(sub_id, command)
    local mcp = require("lib.mcp")
    local sub = self.subscriptions[sub_id]
    local cmd_type = command.type

    if cmd_type == "tools_list" then
        local ctx = sub and sub.caller_context or {}
        self:send({
            subscriptionId = sub_id,
            type = "tools_list",
            tools = mcp.list_tools(ctx.session_uuid),
        })

    elseif cmd_type == "tool_call" then
        local call_id = command.call_id
        local tool_name = command.name
        local params = command.arguments or {}

        local context = sub and sub.caller_context or {}

        mcp.call_tool(tool_name, params, context, function(result, err)
            if err then
                self:send({
                    subscriptionId = sub_id,
                    type = "tool_result",
                    call_id = call_id,
                    is_error = true,
                    content = { { type = "text", text = err } },
                })
            else
                self:send({
                    subscriptionId = sub_id,
                    type = "tool_result",
                    call_id = call_id,
                    is_error = false,
                    content = result,
                })
            end
        end)

    elseif cmd_type == "prompts_list" then
        self:send({
            subscriptionId = sub_id,
            type = "prompts_list",
            prompts = mcp.list_prompts(),
        })

    elseif cmd_type == "prompt_get" then
        local call_id = command.call_id
        local prompt_name = command.name
        local args = command.arguments or {}
        local result, err = mcp.get_prompt(prompt_name, args)
        if err then
            self:send({
                subscriptionId = sub_id,
                type = "prompt_result",
                call_id = call_id,
                name = prompt_name,
                is_error = true,
                content = { { type = "text", text = err } },
            })
        else
            self:send({
                subscriptionId = sub_id,
                type = "prompt_result",
                call_id = call_id,
                name = prompt_name,
                is_error = false,
                description = result.description,
                messages = result.messages,
            })
        end

    elseif cmd_type == "resource_templates_list" then
        self:send({
            subscriptionId = sub_id,
            type = "resource_templates_list",
            resourceTemplates = mcp.list_resource_templates(),
        })

    elseif cmd_type == "resource_read" then
        local call_id = command.call_id
        local uri = command.uri

        local context = sub and sub.caller_context or {}

        mcp.read_resource(uri, context, function(contents, err)
            if err then
                self:send({
                    subscriptionId = sub_id,
                    type = "resource_result",
                    call_id = call_id,
                    is_error = true,
                    content = { { type = "text", text = err } },
                })
            else
                self:send({
                    subscriptionId = sub_id,
                    type = "resource_result",
                    call_id = call_id,
                    is_error = false,
                    contents = contents,
                })
            end
        end)

    else
        log.debug(string.format("Unknown MCP command: %s", tostring(cmd_type)))
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

    -- Unregister from all terminal sessions (auto-resizes to next client)
    for _, sub in pairs(self.subscriptions) do
        if sub.channel == "terminal" and sub.session_uuid then
            pty_clients.unregister(sub.session_uuid, self.peer_id)
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
