-- Connection registry (hot-reloadable)
--
-- Shared client registry for all transports (WebRTC, TUI, future).
-- Manages client lifecycle, broadcasts hub events to all connected clients.
--
-- Each transport handler (webrtc.lua, tui.lua) registers clients here.
-- State is persisted in hub.state across hot-reloads.

local state = require("hub.state")
local Agent = require("lib.agent")

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
    hooks.notify("client_connected", { peer_id = peer_id, transport = client.transport.type })
end

--- Unregister a client from the shared registry.
-- Called by transport handlers when a peer disconnects.
-- @param peer_id The unique peer identifier
local function unregister_client(peer_id)
    local client = clients[peer_id]
    if client then
        hooks.notify("client_disconnected", { peer_id = peer_id, transport = client.transport.type })
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
-- Hook Observers (Lua → Lua)
-- ============================================================================
-- Observe agent lifecycle hooks emitted by handlers/agents.lua.
-- hooks.on() is name-based (overwrites on re-register), so no ID tracking needed.

hooks.on("agent_created", "broadcast_agent_created", function(info)
    log.info(string.format("Broadcasting agent_created: %s",
        info.id or info.session_key or "?"))

    broadcast_hub_event("agent_created", { agent = info })
    broadcast_hub_event("agent_list", { agents = Agent.all_info() })

    local worktrees = hub.get_worktrees()
    broadcast_hub_event("worktree_list", { worktrees = worktrees })
end)

hooks.on("agent_deleted", "broadcast_agent_deleted", function(agent_id)
    log.info(string.format("Broadcasting agent_deleted: %s", agent_id or "?"))

    broadcast_hub_event("agent_deleted", { agent_id = agent_id })
    broadcast_hub_event("agent_list", { agents = Agent.all_info() })

    local worktrees = hub.get_worktrees()
    broadcast_hub_event("worktree_list", { worktrees = worktrees })
end)

-- Send a web push notification when a PTY notification (bell) fires.
-- Builds a deep link to the exact hub/agent/pty session.
-- Customizable: override by calling hooks.off("pty_notification", "push_notification")
-- and registering your own handler with push.send().
hooks.on("pty_notification", "push_notification", function(info)
    local hub_id = hub.server_id()
    local agent = info.agent_key and Agent.get(info.agent_key)

    -- Build deep link: /hubs/:id/agents/:index/ptys/:pty_index
    local url = nil
    if hub_id and agent and agent.agent_index then
        -- Find pty_index from session_order (0-based for URL)
        local pty_index = 0
        for i, entry in ipairs(agent.session_order or {}) do
            if entry.name == info.session_name then
                pty_index = i - 1
                break
            end
        end
        url = string.format("/hubs/%s/agents/%d/ptys/%d", hub_id, agent.agent_index, pty_index)
    elseif hub_id then
        url = string.format("/hubs/%s", hub_id)
    end

    -- Build a readable title from agent metadata
    local title = "Agent alert"
    if agent then
        -- Use short repo name (strip owner prefix) + issue/branch
        local repo_short = agent.repo and agent.repo:match("/(.+)$") or agent.repo
        if agent.issue_number then
            title = string.format("%s #%s", repo_short or "agent", agent.issue_number)
        elseif repo_short then
            title = repo_short
        end
    end
    local body = info.message or info.body or "Your attention is needed"

    push.send({
        kind = "agent_alert",
        title = title,
        body = body,
        url = url,
    })
end)

hooks.on("agent_lifecycle", "broadcast_lifecycle", function(info)
    log.debug(string.format("Broadcasting agent_lifecycle: %s -> %s",
        info.agent_id or "?", info.status or "?"))

    broadcast_hub_event("agent_status_changed", {
        agent_id = info.agent_id,
        status = info.status,
    })
end)

-- ============================================================================
-- Rust Event Handlers (Rust → Lua)
-- ============================================================================
-- These events originate from Rust and are delivered through the events system.
-- Tracked for cleanup on hot-reload (see _before_reload).

local _event_subs = {}

_event_subs[#_event_subs + 1] = events.on("connection_code_ready", function(data)
    log.info("Broadcasting connection_code to hub subscribers")
    broadcast_hub_event("connection_code", { url = data.url, qr_ascii = data.qr_ascii })
end)

_event_subs[#_event_subs + 1] = events.on("connection_code_error", function(err)
    log.warn(string.format("Broadcasting connection_code_error: %s", err or "unknown"))
    broadcast_hub_event("connection_code_error", { error = err or "Connection code not available" })
end)

_event_subs[#_event_subs + 1] = events.on("agent_status_changed", function(info)
    log.debug(string.format("Broadcasting agent_status_changed: %s -> %s",
        info.agent_id or "?", info.status or "?"))

    broadcast_hub_event("agent_status_changed", {
        agent_id = info.agent_id,
        status = info.status,
    })
end)

_event_subs[#_event_subs + 1] = events.on("process_exited", function(data)
    local agent_key = data.agent_key
    local exit_code = data.exit_code
    log.info(string.format("Process exited for %s (code=%s)",
        agent_key or "?", tostring(exit_code)))

    local agent = Agent.get(agent_key)
    if agent then
        agent.status = "exited"
        broadcast_hub_event("agent_status_changed", {
            agent_id = agent_key,
            status = "exited",
        })
    end
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
    -- Unsubscribe Rust event callbacks
    for _, sub_id in ipairs(_event_subs) do
        events.off(sub_id)
    end
    _event_subs = {}
    -- Remove hook observers (re-registered on reload)
    hooks.off("agent_created", "broadcast_agent_created")
    hooks.off("agent_deleted", "broadcast_agent_deleted")
    hooks.off("agent_lifecycle", "broadcast_lifecycle")
    hooks.off("pty_notification", "push_notification")
    log.info(string.format("connections.lua reloading with %d client(s)", get_client_count()))
end

function M._after_reload()
    log.info(string.format("connections.lua reloaded, %d client(s) preserved", get_client_count()))
end

log.info(string.format("Connection registry loaded (%d existing clients)", get_client_count()))

return M
