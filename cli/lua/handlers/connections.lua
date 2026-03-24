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
local AgentListPayload = require("lib.agent_list_payload")

-- Shared client registry - all transports register here
local clients = state.get("connections.clients", {})

-- Connection statistics across all transports
local stats = state.get("connections.stats", {
    total_connections = 0,
    total_messages = 0,
    total_disconnections = 0,
    agent_list_broadcasts = 0,
    agent_list_deduped = 0,
})
local last_agent_list_snapshot = state.get("connections.last_agent_list_snapshot", nil)
local hub_recovery_state = state.get("connections.hub_recovery_state", {
    state = "starting",
})

-- ============================================================================
-- Client Registry
-- ============================================================================

--- Register a client in the shared registry.
-- @param peer_id The unique peer identifier
-- @param client The Client instance (from lib.client)
local function register_client(peer_id, client)
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
        agent_list_broadcasts = stats.agent_list_broadcasts,
        agent_list_deduped = stats.agent_list_deduped,
    }
end

-- ============================================================================
-- Hub Event Broadcasting
-- ============================================================================

--- Broadcast a hub event to all clients with hub channel subscriptions.
-- @param event_name The event name (for logging)
-- @param event_data The data to merge into the message
local function broadcast_hub_event(event_name, event_data)
    -- Coalesce identical agent_list payloads to reduce subscription churn.
    if event_name == "agent_list" then
        local payload = AgentListPayload.build(event_data and event_data.agents or nil)
        event_data = {
            agents = payload.agents,
            workspaces = payload.workspaces,
        }
        local ok, snapshot = pcall(json.encode, event_data)
        if ok then
            if last_agent_list_snapshot == snapshot then
                stats.agent_list_deduped = stats.agent_list_deduped + 1
                log.debug("Deduped agent_list broadcast (payload unchanged)")
                return
            end
            last_agent_list_snapshot = snapshot
            state.set("connections.last_agent_list_snapshot", snapshot)
        end
    end

    local broadcast_count = 0

    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "hub" then
                local message = {
                    subscriptionId = sub_id,
                    type = event_name,
                }
                for k, v in pairs(event_data) do
                    message[k] = v
                end

                client:send(message)
                broadcast_count = broadcast_count + 1
            end
        end
    end

    if broadcast_count > 0 then
        if event_name == "agent_list" then
            stats.agent_list_broadcasts = stats.agent_list_broadcasts + 1
        end
        log.debug(string.format("Broadcast %s to %d subscription(s)", event_name, broadcast_count))
    end
end

local function broadcast_workspace_list()
    local Hub = require("lib.hub")
    local ok, workspaces = pcall(function()
        return Hub.get():list_workspaces()
    end)
    if not ok then
        log.warn(string.format("Failed to broadcast workspace_list: %s", tostring(workspaces)))
        workspaces = {}
    end

    broadcast_hub_event("workspace_list", {
        workspaces = workspaces,
    })
end

local function broadcast_spawn_target_list()
    local broadcast_count = 0

    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "hub" then
                client:send_spawn_target_list(sub_id)
                broadcast_count = broadcast_count + 1
            end
        end
    end

    if broadcast_count > 0 then
        log.debug(string.format("Broadcast spawn_target_list to %d subscription(s)", broadcast_count))
    end
end

-- ============================================================================
-- Hook Observers (Lua → Lua)
-- ============================================================================

hooks.on("agent_created", "broadcast_agent_created", function(info)
    log.info(string.format("Broadcasting agent_created: %s",
        info.id or info.session_uuid or "?"))

    broadcast_hub_event("agent_created", { agent = info })
    broadcast_hub_event("agent_list", { agents = Agent.all_info() })
    broadcast_workspace_list()

    local worktrees = hub.get_worktrees()
    broadcast_hub_event("worktree_list", { worktrees = worktrees })
end)

hooks.on("agent_deleted", "broadcast_agent_deleted", function(agent_id)
    log.info(string.format("Broadcasting agent_deleted: %s", agent_id or "?"))

    -- Cancel idle timer for the deleted session.
    if agent_id then
        timer.cancel("idle:" .. agent_id)
    end

    broadcast_hub_event("agent_deleted", { agent_id = agent_id })
    broadcast_hub_event("agent_list", { agents = Agent.all_info() })
    broadcast_workspace_list()

    local worktrees = hub.get_worktrees()
    broadcast_hub_event("worktree_list", { worktrees = worktrees })
end)

-- Global callable by Rust to update per-client focus state.
-- Rust calls this with (session_uuid, peer_id, focused).
function _set_pty_focused(session_uuid, peer_id, focused)
    if session_uuid then
        pty_clients.set_focused(session_uuid, peer_id, focused)
    end
end

-- Enrich raw PTY notifications from Rust with agent state, then re-dispatch.
hooks.on("_pty_notification_raw", "enrich_and_dispatch", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    info.already_notified = agent and agent.notification or false

    -- Check if any client is actively viewing this session
    info.has_focus = agent and agent.session_uuid
        and pty_clients.is_any_focused(agent.session_uuid) or false

    -- Include session_uuid for downstream consumers
    info.session_uuid = agent and agent.session_uuid or nil

    hooks.notify("pty_notification", info)
end)

-- Send a web push notification when a PTY notification (bell) fires.
hooks.on("pty_notification", "push_notification", function(info)
    if info.has_focus then return end
    if info.already_notified then return end

    local hub_id = hub.server_id()
    local agent = (info.session_uuid and Agent.get(info.session_uuid))

    -- Build deep link using session_uuid
    local url = nil
    if hub_id and agent and agent.session_uuid then
        url = string.format("/hubs/%s/sessions/%s", hub_id, agent.session_uuid)
    elseif hub_id then
        url = string.format("/hubs/%s", hub_id)
    end

    local title = "Agent alert"
    if agent then
        local repo_short = agent.repo and agent.repo:match("/(.+)$") or agent.repo
        if agent:get_meta("issue_number") then
            title = string.format("%s #%s", repo_short or "agent", agent:get_meta("issue_number"))
        elseif repo_short then
            title = repo_short
        end
    end
    local body = info.message or info.body or "Your attention is needed"

    -- Set notification flag (session_updated hook broadcasts agent_list)
    if agent then
        agent:update({ notification = true })
    end

    local badge_count = 0
    for _, a in ipairs(Agent.list()) do
        if a.notification then badge_count = badge_count + 1 end
    end

    push.send({
        kind = "agent_alert",
        title = title,
        body = body,
        url = url,
        sessionUuid = agent and agent.session_uuid or nil,
        app_badge = badge_count,
    })

    broadcast_hub_event("pty_notification", { title = title, body = body })
end)

-- Clear a pending notification on a session by session_uuid.
local function clear_session_notification(session_uuid)
    local agent = Agent.get(session_uuid)
    local cleared = false
    if agent and agent.notification then
        agent:update({ notification = false })
        cleared = true
    end
    local any_remaining = false
    for _, a in ipairs(Agent.list()) do
        if a.notification then any_remaining = true; break end
    end
    return cleared, any_remaining, agent
end

-- Called directly from Rust on the PTY input hot path.
-- Rust passes session_uuid; we look up the agent for hook dispatch.
function _on_pty_input(session_uuid)
    if not session_uuid then return false end
    local agent = Agent.get(session_uuid)

    local cleared, any_remaining = clear_session_notification(session_uuid)
    if cleared and agent then
        hooks.notify("pty_input", { session_uuid = session_uuid })
    end
    return any_remaining
end

-- Exported for the clear_notification command (TUI agent switching).
function _clear_session_notification(session_uuid)
    local _, any_remaining = clear_session_notification(session_uuid)
    return any_remaining
end

-- Update agent title when the running program sets the terminal title (OSC 0/2).
hooks.on("pty_title_changed", "update_agent_title", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    if agent then
        agent:update({ title = info.title })
    end
end)

-- Update agent CWD when the shell reports a directory change (OSC 7).
hooks.on("pty_cwd_changed", "update_agent_cwd", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    if agent then
        agent:update({ cwd = info.cwd })
    end
end)

-- Track shell integration prompt marks (OSC 133/633).
hooks.on("pty_prompt", "update_agent_prompt", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    if agent then
        agent.last_prompt_mark = info
    end
end)

-- Track cursor visibility changes (DECTCEM CSI ? 25 h/l).
hooks.on("pty_cursor_visibility", "update_agent_cursor", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    if agent then
        agent.cursor_visible = info.visible
    end
end)

-- ============================================================================
-- Idle / Active Detection
-- ============================================================================
-- Idle detection: event-driven via timer.after_idle (no polling).
--
-- Each pty_output event resets a per-session idle timer. If no output
-- arrives within IDLE_THRESHOLD_SECS, the timer fires and sets is_idle = true.
-- When output resumes after idle, is_idle is cleared immediately.
--
-- timer.after_idle is a Rust primitive: spawns a tokio::time::sleep task
-- that is aborted and replaced on each reset. Zero polling.

local IDLE_THRESHOLD_SECS = 5

hooks.on("pty_output", "idle_activity_reset", function(ctx, _data)
    local uuid = ctx.session_uuid
    if not uuid then return end

    local session = Agent.get(uuid)
    if not session then return end

    -- Mark active (session:update triggers session_updated → broadcast)
    if session.is_idle then
        session:update({ is_idle = false })
    end

    -- Reset the idle timer — fires only after IDLE_THRESHOLD_SECS of silence
    timer.after_idle("idle:" .. uuid, IDLE_THRESHOLD_SECS, function()
        local s = Agent.get(uuid)
        if s and not s.is_idle then
            s:update({ is_idle = true })
        end
    end)
end)

-- Auto-broadcast agent list when any session field changes.
hooks.on("session_updated", "broadcast_session_updated", function()
    last_agent_list_snapshot = nil
    broadcast_hub_event("agent_list", { agents = Agent.all_info() })
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

local _event_subs = {}

_event_subs[#_event_subs + 1] = events.on("connection_code_ready", function(data)
    log.info("Broadcasting connection_code to hub subscribers")
    broadcast_hub_event("connection_code", { url = data.url, qr_ascii = data.qr_ascii })
end)

_event_subs[#_event_subs + 1] = events.on("connection_code_error", function(err)
    log.warn(string.format("Broadcasting connection_code_error: %s", err or "unknown"))
    broadcast_hub_event("connection_code_error", { error = err or "Connection code not available" })
end)

_event_subs[#_event_subs + 1] = events.on("hub_recovery_state", function(info)
    local incoming = (type(info) == "table") and info or {}

    -- Replace the persisted table in place so late subscribers can request
    -- the exact latest lifecycle payload.
    for k in pairs(hub_recovery_state) do
        hub_recovery_state[k] = nil
    end
    for k, v in pairs(incoming) do
        hub_recovery_state[k] = v
    end
    hub_recovery_state.state = hub_recovery_state.state or "starting"
    state.set("connections.hub_recovery_state", hub_recovery_state)

    broadcast_hub_event("hub_recovery_state", hub_recovery_state)
    if hub_recovery_state.state == "ready" then
        broadcast_hub_event("hub_ready", hub_recovery_state)
    end
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
    local session_uuid = data.session_uuid
    local exit_code = data.exit_code
    log.info(string.format("Process exited for %s (code=%s)",
        session_uuid or "?", tostring(exit_code)))

    local agent = (session_uuid and Agent.get(session_uuid))
    if agent then
        agent:update({ status = "exited" })
        broadcast_hub_event("agent_status_changed", {
            agent_id = agent.session_uuid,
            status = "exited",
        })
    end
end)

-- Notify MCP clients when tool list changes
_event_subs[#_event_subs + 1] = events.on("mcp_tools_changed", function()
    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "mcp" then
                client:send({
                    subscriptionId = sub_id,
                    type = "tools_list_changed",
                })
            end
        end
    end
end)

-- Notify MCP clients when prompt list changes
_event_subs[#_event_subs + 1] = events.on("mcp_prompts_changed", function()
    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "mcp" then
                client:send({
                    subscriptionId = sub_id,
                    type = "prompts_list_changed",
                })
            end
        end
    end
end)

-- Notify MCP clients when resource template list changes
_event_subs[#_event_subs + 1] = events.on("mcp_resources_changed", function()
    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions) do
            if sub.channel == "mcp" then
                client:send({
                    subscriptionId = sub_id,
                    type = "resources_list_changed",
                })
            end
        end
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
    broadcast_workspace_list = broadcast_workspace_list,
    broadcast_spawn_target_list = broadcast_spawn_target_list,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    for _, sub_id in ipairs(_event_subs) do
        events.off(sub_id)
    end
    _event_subs = {}
    hooks.off("agent_created", "broadcast_agent_created")
    hooks.off("agent_deleted", "broadcast_agent_deleted")
    hooks.off("agent_lifecycle", "broadcast_lifecycle")
    hooks.off("_pty_notification_raw", "enrich_and_dispatch")
    hooks.off("pty_notification", "push_notification")
    hooks.off("pty_title_changed", "update_agent_title")
    hooks.off("pty_cwd_changed", "update_agent_cwd")
    hooks.off("pty_output", "idle_activity_reset")
    hooks.off("session_updated", "broadcast_session_updated")
    hooks.off("pty_prompt", "update_agent_prompt")
    hooks.off("pty_cursor_visibility", "update_agent_cursor")
    _set_pty_focused = nil
    _on_pty_input = nil
    _clear_session_notification = nil
    log.info(string.format("connections.lua reloading with %d client(s)", get_client_count()))
end

function M._after_reload()
    log.info(string.format("connections.lua reloaded, %d client(s) preserved", get_client_count()))
end

log.info(string.format("Connection registry loaded (%d existing clients)", get_client_count()))

return M
