-- Connection registry (hot-reloadable)
--
-- Shared client registry for all transports (WebRTC, TUI, future).
-- Manages client lifecycle, broadcasts hub events to all connected clients.
--
-- Each transport handler (webrtc.lua, tui.lua) registers clients here.
-- State is persisted in core.state across hot-reloads.

local state = require("core.state")

-- Shared client registry - all transports register here
local clients = state.get("connections.clients", {})

-- Connection statistics across all transports
local stats = state.get("connections.stats", {
    total_connections = 0,
    total_messages = 0,
    total_disconnections = 0,
})

-- ============================================================================
-- Client Registry
-- ============================================================================

--- Register a client in the shared registry.
-- Called by transport handlers (webrtc.lua, tui.lua) when a peer connects.
-- @param peer_id The unique peer identifier
-- @param client The Client instance (from lib.client)
local function register_client(peer_id, client)
    -- Clean up stale client with same ID (e.g., browser refresh)
    local old_client = clients[peer_id]
    if old_client then
        log.debug(string.format("Cleaning up stale client: %s...", peer_id:sub(1, 8)))
        old_client:disconnect()
    end

    clients[peer_id] = client
    stats.total_connections = stats.total_connections + 1
end

--- Unregister a client from the shared registry.
-- Called by transport handlers when a peer disconnects.
-- @param peer_id The unique peer identifier
local function unregister_client(peer_id)
    local client = clients[peer_id]
    if client then
        client:disconnect()
        clients[peer_id] = nil
    end

    stats.total_disconnections = stats.total_disconnections + 1
end

--- Get a client by peer ID.
-- @param peer_id The unique peer identifier
-- @return The Client instance, or nil
local function get_client(peer_id)
    return clients[peer_id]
end

--- Track a message received from any transport.
local function track_message()
    stats.total_messages = stats.total_messages + 1
end

--- Get the number of active clients across all transports.
-- @return Number of connected clients
local function get_client_count()
    local count = 0
    for _ in pairs(clients) do
        count = count + 1
    end
    return count
end

--- Get connection statistics.
-- @return Statistics table
local function get_stats()
    return {
        active_clients = get_client_count(),
        total_connections = stats.total_connections,
        total_messages = stats.total_messages,
        total_disconnections = stats.total_disconnections,
    }
end

-- ============================================================================
-- Hub Event Broadcasting
-- ============================================================================

--- Broadcast a hub event to all clients with hub channel subscriptions.
-- @param event_name The event name (for logging)
-- @param event_data The data to merge into the message
local function broadcast_hub_event(event_name, event_data)
    local broadcast_count = 0

    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "hub" then
                local message = {
                    subscriptionId = sub_id,
                    type = event_name,
                }
                -- Merge event_data into message
                for k, v in pairs(event_data) do
                    message[k] = v
                end

                client:send(message)
                broadcast_count = broadcast_count + 1
            end
        end
    end

    if broadcast_count > 0 then
        log.debug(string.format("Broadcast %s to %d subscription(s)", event_name, broadcast_count))
    end
end

-- ============================================================================
-- Hub Event Handlers
-- ============================================================================
-- Broadcast agent lifecycle events to all connected clients.

events.on("agent_created", function(info)
    log.info(string.format("Broadcasting agent_created: %s",
        info.id or info.session_key or "?"))

    broadcast_hub_event("agent_created", { agent = info })

    -- Worktree list changed (new agent uses a worktree)
    local worktrees = hub.get_worktrees()
    broadcast_hub_event("worktree_list", { worktrees = worktrees })
end)

events.on("agent_deleted", function(agent_id)
    log.info(string.format("Broadcasting agent_deleted: %s", agent_id or "?"))

    broadcast_hub_event("agent_deleted", { agent_id = agent_id })

    -- Worktree list changed (deleted agent may free a worktree)
    local worktrees = hub.get_worktrees()
    broadcast_hub_event("worktree_list", { worktrees = worktrees })
end)

events.on("connection_code_ready", function(data)
    log.info("Broadcasting connection_code to hub subscribers")
    broadcast_hub_event("connection_code", { url = data.url, qr_ascii = data.qr_ascii })
end)

events.on("connection_code_error", function(err)
    log.warn(string.format("Broadcasting connection_code_error: %s", err or "unknown"))
    broadcast_hub_event("connection_code_error", { error = err or "Connection code not available" })
end)

events.on("agent_status_changed", function(info)
    log.debug(string.format("Broadcasting agent_status_changed: %s -> %s",
        info.agent_id or "?", info.status or "?"))

    broadcast_hub_event("agent_status_changed", {
        agent_id = info.agent_id,
        status = info.status,
    })
end)

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {
    register_client = register_client,
    unregister_client = unregister_client,
    get_client = get_client,
    track_message = track_message,
    get_client_count = get_client_count,
    get_stats = get_stats,
    broadcast_hub_event = broadcast_hub_event,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info(string.format("connections.lua reloading with %d client(s)", get_client_count()))
end

function M._after_reload()
    log.info(string.format("connections.lua reloaded, %d client(s) preserved", get_client_count()))
end

log.info(string.format("Connection registry loaded (%d existing clients)", get_client_count()))

return M
