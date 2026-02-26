-- @template Orchestrator
-- @description Connect to other hubs and manage agents remotely
-- @category plugins
-- @dest shared/plugins/orchestrator/init.lua
-- @scope device
-- @version 1.0.0

-- Orchestrator plugin
--
-- Discovers running hubs on the local machine, connects to them via
-- Unix socket, and maintains a live registry of their agents. Exposes
-- hook-based APIs that an orchestrator agent (or other plugins) can
-- call to list hubs, create agents on remote hubs, and delete them.
--
-- Uses two Lua primitives:
--   hub_discovery — list(), is_running(hub_id), socket_path(hub_id)
--   hub_client    — connect(path), on_message(conn, cb), send(conn, data), close(conn)

local hooks = require("hub.hooks")

local self_id = hub.server_id()

-- ============================================================================
-- Hub Registry
-- ============================================================================

-- hub_id -> { conn_id, agents, status }
local connected_hubs = {}

-- conn_id -> hub_id (reverse lookup for disconnect cleanup)
local conn_to_hub = {}

-- ============================================================================
-- Message Handling
-- ============================================================================

--- Process a JSON message received from a remote hub.
-- Updates the local registry so hook consumers always see fresh state.
--
-- @param hub_id string The remote hub's ID
-- @param message table Decoded JSON message from the remote hub
local function handle_hub_message(hub_id, message)
    local hub_entry = connected_hubs[hub_id]
    if not hub_entry then return end

    local msg_type = message.type

    if msg_type == "subscribed" then
        hub_entry.status = "connected"
        log.info("Orchestrator: connected to hub " .. hub_id)

    elseif msg_type == "agent_list" then
        hub_entry.agents = message.agents or {}
        events.emit("remote_agents_updated", {
            hub_id = hub_id,
            agents = hub_entry.agents,
        })

    elseif msg_type == "agent_created" then
        -- The hub will broadcast a fresh agent_list after creation,
        -- but we can emit an event immediately for fast UI feedback.
        events.emit("remote_agent_created", {
            hub_id = hub_id,
            agent = message.agent,
        })

    elseif msg_type == "agent_deleted" then
        events.emit("remote_agent_deleted", {
            hub_id = hub_id,
            agent_id = message.agent_id,
        })

    elseif msg_type == "agent_status_changed" then
        -- Update the cached agent status if we have it
        if hub_entry.agents and message.agent_id then
            for _, agent in ipairs(hub_entry.agents) do
                if agent.key == message.agent_id then
                    agent.status = message.status
                    break
                end
            end
        end
        events.emit("remote_agent_status_changed", {
            hub_id = hub_id,
            agent_id = message.agent_id,
            status = message.status,
        })

    elseif msg_type == "error" then
        log.warn(string.format("Orchestrator: error from hub %s: %s",
            hub_id, tostring(message.message or message.error)))
    end
end

-- ============================================================================
-- Connection Management
-- ============================================================================

--- Connect to a discovered hub and wire up message handling.
--
-- @param hub_id string The remote hub's server ID
-- @param socket_path string Path to the remote hub's Unix socket
local function connect_to_hub(hub_id, socket_path)
    if connected_hubs[hub_id] then return end

    log.info(string.format("Orchestrator: connecting to hub %s at %s", hub_id, socket_path))

    local conn_id = hub_client.connect(socket_path)

    connected_hubs[hub_id] = {
        conn_id = conn_id,
        agents = {},
        status = "connecting",
    }
    conn_to_hub[conn_id] = hub_id

    hub_client.on_message(conn_id, function(message, connection_id)
        handle_hub_message(hub_id, message)
    end)
end

--- Disconnect from a remote hub and clean up registry state.
--
-- @param hub_id string The remote hub's server ID
local function disconnect_hub(hub_id)
    local hub_entry = connected_hubs[hub_id]
    if not hub_entry then return end

    log.info("Orchestrator: disconnecting from hub " .. hub_id)

    hub_client.close(hub_entry.conn_id)
    conn_to_hub[hub_entry.conn_id] = nil
    connected_hubs[hub_id] = nil

    events.emit("remote_hub_disconnected", { hub_id = hub_id })
end

--- Mark a hub as disconnected without sending a close (the socket is already gone).
--
-- @param hub_id string The remote hub's server ID
local function mark_disconnected(hub_id)
    local hub_entry = connected_hubs[hub_id]
    if not hub_entry then return end

    log.info("Orchestrator: hub " .. hub_id .. " is no longer reachable")

    conn_to_hub[hub_entry.conn_id] = nil
    connected_hubs[hub_id] = nil

    events.emit("remote_hub_disconnected", { hub_id = hub_id })
end

-- ============================================================================
-- Discovery
-- ============================================================================

--- Scan for running hubs and connect to any we haven't seen yet.
-- Also prunes hubs that are no longer running.
local function discover_and_connect()
    -- Prune hubs that stopped running
    for hub_id, _ in pairs(connected_hubs) do
        if not hub_discovery.is_running(hub_id) then
            mark_disconnected(hub_id)
        end
    end

    -- Connect to newly discovered hubs
    local running = hub_discovery.list()
    for _, info in ipairs(running) do
        if info.id ~= self_id and not connected_hubs[info.id] then
            connect_to_hub(info.id, info.socket)
        end
    end
end

-- ============================================================================
-- Hook-based API
-- ============================================================================

--- List all known remote hubs and their agents.
-- Returns an array of { id, status, agent_count, agents }.
hooks.on("orchestrator_list_hubs", "orchestrator_list_hubs_impl", function()
    local result = {}
    for hub_id, hub_entry in pairs(connected_hubs) do
        table.insert(result, {
            id = hub_id,
            status = hub_entry.status,
            agent_count = #(hub_entry.agents or {}),
            agents = hub_entry.agents,
        })
    end
    return result
end)

--- Create an agent on a remote hub.
-- data: { hub_id, issue_or_branch, prompt, profile }
hooks.on("orchestrator_create_agent", "orchestrator_create_agent_impl", function(data)
    if not data or not data.hub_id then
        log.warn("Orchestrator: create_agent called without hub_id")
        return
    end

    local hub_entry = connected_hubs[data.hub_id]
    if not hub_entry then
        log.warn("Orchestrator: hub not connected: " .. tostring(data.hub_id))
        return
    end

    hub_client.send(hub_entry.conn_id, {
        subscriptionId = "orchestrator",
        type = "create_agent",
        issue_or_branch = data.issue_or_branch,
        prompt = data.prompt,
        profile = data.profile,
    })
    log.info(string.format("Orchestrator: requested agent creation on hub %s", data.hub_id))
end)

--- Delete an agent on a remote hub.
-- data: { hub_id, agent_id, delete_worktree }
hooks.on("orchestrator_delete_agent", "orchestrator_delete_agent_impl", function(data)
    if not data or not data.hub_id or not data.agent_id then
        log.warn("Orchestrator: delete_agent called without hub_id or agent_id")
        return
    end

    local hub_entry = connected_hubs[data.hub_id]
    if not hub_entry then
        log.warn("Orchestrator: hub not connected: " .. tostring(data.hub_id))
        return
    end

    hub_client.send(hub_entry.conn_id, {
        subscriptionId = "orchestrator",
        type = "delete_agent",
        id = data.agent_id,
        delete_worktree = data.delete_worktree or false,
    })
    log.info(string.format("Orchestrator: requested agent deletion %s on hub %s",
        data.agent_id, data.hub_id))
end)

--- Request a fresh agent list from a specific remote hub.
-- data: { hub_id }
hooks.on("orchestrator_refresh_agents", "orchestrator_refresh_agents_impl", function(data)
    if not data or not data.hub_id then return end

    local hub_entry = connected_hubs[data.hub_id]
    if not hub_entry then return end

    hub_client.send(hub_entry.conn_id, {
        subscriptionId = "orchestrator",
        type = "list_agents",
    })
end)

--- Disconnect from a specific remote hub.
-- data: { hub_id }
hooks.on("orchestrator_disconnect_hub", "orchestrator_disconnect_hub_impl", function(data)
    if not data or not data.hub_id then return end
    disconnect_hub(data.hub_id)
end)

-- ============================================================================
-- Lifecycle
-- ============================================================================

-- Initial discovery on load
discover_and_connect()

-- Periodic scan: discover new hubs, prune dead ones (every 30 seconds)
timer.every(30, discover_and_connect)

local hub_count = 0
for _ in pairs(connected_hubs) do hub_count = hub_count + 1 end
log.info(string.format("Orchestrator plugin loaded (self=%s, connected to %d hubs)",
    tostring(self_id), hub_count))

return {}
