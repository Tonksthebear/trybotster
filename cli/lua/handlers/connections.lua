-- Connection registry (hot-reloadable)
--
-- Shared client registry for all transports (WebRTC, TUI, future).
-- Manages client lifecycle, broadcasts hub events to all connected clients.
--
-- Each transport handler (webrtc.lua, tui.lua) registers clients here.
-- State is persisted in hub.state across hot-reloads.

local state = require("hub.state")
local Agent = require("lib.agent")
local pty_clients = require("lib.pty_clients")

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

-- Global callable by Rust to update per-client focus state.
-- Rust calls this when it detects focus-in/focus-out sequences in PTY input.
function _set_pty_focused(agent_index, pty_index, peer_id, focused)
    pty_clients.set_focused(agent_index, pty_index, peer_id, focused)
end

-- Enrich raw PTY notifications from Rust with agent state, then re-dispatch
-- as the public "pty_notification" event. Consumers get `already_notified`,
-- `has_focus`, and `pty_index` without needing to query anything themselves.
hooks.on("_pty_notification_raw", "enrich_and_dispatch", function(info)
    local agent = info.agent_key and Agent.get(info.agent_key)
    info.already_notified = agent and agent.notification or false

    -- Resolve pty_index from session_order (0-based)
    local pty_index = 0
    if agent then
        for i, entry in ipairs(agent.session_order or {}) do
            if entry.name == info.session_name then
                pty_index = i - 1
                break
            end
        end
    end
    info.pty_index = pty_index

    -- Check if any client is actively viewing this PTY
    info.has_focus = agent and agent.agent_index
        and pty_clients.is_any_focused(agent.agent_index, pty_index) or false

    hooks.notify("pty_notification", info)
end)

-- Send a web push notification when a PTY notification (bell) fires.
-- Builds a deep link to the exact hub/agent/pty session.
-- Customizable: override by calling hooks.off("pty_notification", "push_notification")
-- and registering your own handler with push.send().
hooks.on("pty_notification", "push_notification", function(info)
    if info.has_focus then return end
    if info.already_notified then return end

    local hub_id = hub.server_id()
    local agent = info.agent_key and Agent.get(info.agent_key)

    -- Build deep link: /hubs/:id/agents/:index/ptys/:pty_index
    local url = nil
    if hub_id and agent and agent.agent_index then
        url = string.format("/hubs/%s/agents/%d/ptys/%d", hub_id, agent.agent_index, info.pty_index)
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

    -- Set notification flag on agent and broadcast updated list.
    -- Done before push.send() so the badge appears regardless of push delivery.
    if agent then
        agent.notification = true
        broadcast_hub_event("agent_list", { agents = Agent.all_info() })
    end

    -- Count agents with active notifications for app badge
    local badge_count = 0
    for _, a in ipairs(Agent.list()) do
        if a.notification then badge_count = badge_count + 1 end
    end

    push.send({
        kind = "agent_alert",
        title = title,
        body = body,
        url = url,
        agentIndex = agent and agent.agent_index or nil,
        ptyIndex = info.pty_index,
        app_badge = badge_count,
    })

    -- Broadcast to all clients (TUI + browser) so they can handle natively.
    broadcast_hub_event("pty_notification", { title = title, body = body })
end)

-- Clear a pending notification on an agent by index.
-- Shared by both PTY input (Rust hot path) and clear_notification command (TUI agent switch).
-- Returns true if any agent still has notifications, false otherwise.
local function clear_agent_notification(agent_index)
    local agents = Agent.list()
    local agent = agents[agent_index + 1]  -- 0-based, 1-based Lua
    local cleared = false
    if agent and agent.notification then
        agent.notification = false
        broadcast_hub_event("agent_list", { agents = Agent.all_info() })
        cleared = true
    end
    -- Check if any agent still has a pending notification
    local any_remaining = false
    for _, a in ipairs(agents) do
        if a.notification then any_remaining = true; break end
    end
    return cleared, any_remaining, agent
end

-- Called directly from Rust on the PTY input hot path.
-- Rust gates calls with a bool flag — this only runs when a notification
-- is actually pending. Returns true if any agent still has notifications
-- (keeps Rust listening), false to disarm until the next notification.
--
-- Fires hooks.notify("pty_input", ...) so plugins can react to
-- notifications cleared by typing (not by agent switching).
function _on_pty_input(agent_index)
    local cleared, any_remaining, agent = clear_agent_notification(agent_index)
    if cleared and agent then
        hooks.notify("pty_input", { agent_index = agent_index, agent_key = agent._agent_key })
    end
    return any_remaining
end

-- Also exported for the clear_notification command (TUI agent switching).
-- Does NOT fire the pty_input hook since no input occurred.
function _clear_agent_notification(agent_index)
    local _, any_remaining = clear_agent_notification(agent_index)
    return any_remaining
end

-- Update agent title when the running program sets the terminal title (OSC 0/2).
-- This drives the agent display_name in both TUI and browser agent lists.
hooks.on("pty_title_changed", "update_agent_title", function(info)
    local agent = info.agent_key and Agent.get(info.agent_key)
    if agent then
        agent.title = info.title
        broadcast_hub_event("agent_list", { agents = Agent.all_info() })
    end
end)

-- Update agent CWD when the shell reports a directory change (OSC 7).
hooks.on("pty_cwd_changed", "update_agent_cwd", function(info)
    local agent = info.agent_key and Agent.get(info.agent_key)
    if agent then
        agent.cwd = info.cwd
        broadcast_hub_event("agent_list", { agents = Agent.all_info() })
    end
end)

-- Track shell integration prompt marks (OSC 133/633).
-- Stores the last mark on the agent for future use (command tracking, etc.).
hooks.on("pty_prompt", "update_agent_prompt", function(info)
    local agent = info.agent_key and Agent.get(info.agent_key)
    if agent then
        agent.last_prompt_mark = info
    end
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
    hooks.off("_pty_notification_raw", "enrich_and_dispatch")
    hooks.off("pty_notification", "push_notification")
    -- Remove global callable (re-registered on reload)
    _set_pty_focused = nil
    log.info(string.format("connections.lua reloading with %d client(s)", get_client_count()))
end

function M._after_reload()
    log.info(string.format("connections.lua reloaded, %d client(s) preserved", get_client_count()))
end

log.info(string.format("Connection registry loaded (%d existing clients)", get_client_count()))

return M
