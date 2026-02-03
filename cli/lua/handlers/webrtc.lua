-- WebRTC message handler (hot-reloadable)
--
-- Manages WebRTC peer connections and routes messages to Client instances.
-- Each browser peer gets a Client that tracks subscriptions and forwarders.
-- State is persisted in core.state across hot-reloads.

local state = require("core.state")
local Client = require("lib.client")

-- Persist clients across hot-reloads
local clients = state.get("webrtc.clients", {})

-- Track connection statistics
local stats = state.get("webrtc.stats", {
    total_connections = 0,
    total_messages = 0,
    total_disconnections = 0,
})

-- ============================================================================
-- WebRTC Peer Callbacks
-- ============================================================================

-- Called when WebRTC peer connects (ICE complete, DataChannel ready)
webrtc.on_peer_connected(function(peer_id)
    log.info(string.format("Peer connected: %s...", peer_id:sub(1, 8)))

    -- Clean up any stale client with same ID (browser refresh scenario)
    local old_client = clients[peer_id]
    if old_client then
        log.debug(string.format("Cleaning up stale client: %s...", peer_id:sub(1, 8)))
        old_client:disconnect()
    end

    -- Create new client
    clients[peer_id] = Client.new(peer_id)
    stats.total_connections = stats.total_connections + 1
end)

-- Called when WebRTC peer disconnects
webrtc.on_peer_disconnected(function(peer_id)
    log.info(string.format("Peer disconnected: %s...", peer_id:sub(1, 8)))

    local client = clients[peer_id]
    if client then
        client:disconnect()
        clients[peer_id] = nil
    end

    stats.total_disconnections = stats.total_disconnections + 1
end)

-- Called for each decrypted WebRTC message
webrtc.on_message(function(peer_id, msg)
    local client = clients[peer_id]

    if not client then
        -- Client not found. This can happen in two cases:
        -- 1. Peer connected before Lua callbacks were registered (startup race)
        -- 2. Browser refresh where disconnect/reconnect happens quickly
        --
        -- NOTE: There's a potential race condition here. If on_peer_disconnected
        -- is delayed (e.g., WebRTC cleanup is slow), we might create a new client
        -- while the old one's forwarders are still running. This is mitigated by:
        -- - on_peer_connected checking for and cleaning up stale clients (line 29-33)
        -- - Forwarders being keyed by peer_id, so old ones get replaced
        --
        -- If issues arise, consider querying Rust for authoritative peer state.
        log.warn(string.format("Message from unknown peer %s..., creating client",
            peer_id:sub(1, 8)))
        client = Client.new(peer_id)
        clients[peer_id] = client
        stats.total_connections = stats.total_connections + 1
    end

    -- Track message count
    stats.total_messages = stats.total_messages + 1

    -- Route message to client (with error handling)
    local ok, err = pcall(client.on_message, client, msg)
    if not ok then
        -- Log full error server-side for debugging
        log.error(string.format("Error handling message from %s...: %s",
            peer_id:sub(1, 8), tostring(err)))
        -- Send generic error to browser (don't expose internals)
        webrtc.send(peer_id, {
            type = "error",
            error = "Internal error processing message",
        })
    end
end)

-- ============================================================================
-- Hub Event Handlers
-- ============================================================================
-- Broadcast agent lifecycle events to all connected clients.
-- These fire when agents are created, deleted, or change status.

--- Find all HubChannel subscriptions for a client and broadcast an event.
-- @param event_name The event name (for logging)
-- @param event_data The data to send
local function broadcast_hub_event(event_name, event_data)
    local broadcast_count = 0

    for peer_id, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "HubChannel" then
                local message = {
                    subscriptionId = sub_id,
                    event = event_name,
                }
                -- Merge event_data into message
                for k, v in pairs(event_data) do
                    message[k] = v
                end

                webrtc.send(peer_id, message)
                broadcast_count = broadcast_count + 1
            end
        end
    end

    if broadcast_count > 0 then
        log.debug(string.format("Broadcast %s to %d subscription(s)", event_name, broadcast_count))
    end
end

-- Agent created event - broadcast to all HubChannel subscribers
events.on("agent_created", function(info)
    log.info(string.format("Broadcasting agent_created: %s",
        info.id or info.session_key or "?"))

    broadcast_hub_event("agent_created", { agent = info })
end)

-- Agent deleted event - broadcast to all HubChannel subscribers
events.on("agent_deleted", function(agent_id)
    log.info(string.format("Broadcasting agent_deleted: %s", agent_id or "?"))

    broadcast_hub_event("agent_deleted", { agent_id = agent_id })
end)

-- Agent status changed event - broadcast to all HubChannel subscribers
events.on("agent_status_changed", function(info)
    log.debug(string.format("Broadcasting agent_status_changed: %s -> %s",
        info.agent_id or "?", info.status or "?"))

    broadcast_hub_event("agent_status_changed", {
        agent_id = info.agent_id,
        status = info.status,
    })
end)

-- ============================================================================
-- Utility Functions
-- ============================================================================

--- Get the number of active clients.
-- @return Number of connected clients
local function get_client_count()
    local count = 0
    for _ in pairs(clients) do
        count = count + 1
    end
    return count
end

--- Get statistics about the WebRTC handler.
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
-- Module Interface
-- ============================================================================

local M = {
    -- Expose utility functions for debugging/introspection
    get_client_count = get_client_count,
    get_stats = get_stats,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    local count = get_client_count()
    log.info(string.format("webrtc.lua reloading with %d client(s)", count))
end

function M._after_reload()
    local count = get_client_count()
    log.info(string.format("webrtc.lua reloaded, %d client(s) preserved", count))
    log.debug(string.format("Stats: %d connections, %d messages",
        stats.total_connections, stats.total_messages))
end

-- Log module load
log.info("WebRTC handler loaded")

return M
