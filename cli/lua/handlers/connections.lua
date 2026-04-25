-- Connection registry (hot-reloadable)
--
-- Shared client registry for all transports (WebRTC, TUI, future).
-- Manages client lifecycle and broadcasts hub events to all connected
-- clients.
--
-- Wire protocol:
--   The dispatcher emits only:
--     - `entity_snapshot` / `entity_upsert` / `entity_patch` /
--       `entity_remove`  (via lib.entity_broadcast)
--     - `ui_tree_snapshot`  (via lib.tree_snapshot)
--     - `ui_route_registry`  (via Client:send_ui_route_registry)
--     - `transient_event`   (built inline below for pty notifications)
--   Hooks like `agent_created` / `agent_deleted` / `session_updated`
--   are local Lua identifiers; their handlers route through EB.
--   Selection lives on the client — both renderers maintain their own.
--
-- Each transport handler (webrtc.lua, tui.lua) registers clients here.
-- State is persisted in hub.state across hot-reloads.

local state = require("hub.state")
local Agent = require("lib.agent")
local ClientSessionPayload = require("lib.client_session_payload")
local Session = require("lib.session")
local pty_clients = require("lib.pty_clients")
local EB = require("lib.entity_broadcast")

-- Shared client registry - all transports register here
local clients = state.get("connections.clients", {})

-- Connection statistics across all transports
local stats = state.get("connections.stats", {
    total_connections = 0,
    total_messages = 0,
    total_disconnections = 0,
})
local hub_recovery_state = state.get("connections.hub_recovery_state", {
    state = "starting",
})
local last_connection_code = state.get("connections.last_connection_code", nil)
local pending_osc_session_updates = state.get("connections.pending_osc_session_updates", {})

local OSC_SESSION_UPDATE_DEBOUNCE_SECS = 0.5

-- ============================================================================
-- Client Registry
-- ============================================================================

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

local function unregister_client(peer_id)
    local client = clients[peer_id]
    if client then
        hooks.notify("client_disconnected", { peer_id = peer_id, transport = client.transport.type })
        client:disconnect()
        clients[peer_id] = nil
    end

    stats.total_disconnections = stats.total_disconnections + 1
end

local function get_client(peer_id)
    return clients[peer_id]
end

local function track_message()
    stats.total_messages = stats.total_messages + 1
end

local function get_client_count()
    local count = 0
    for _ in pairs(clients) do
        count = count + 1
    end
    return count
end

local function get_stats()
    return {
        active_clients = get_client_count(),
        total_connections = stats.total_connections,
        total_messages = stats.total_messages,
        total_disconnections = stats.total_disconnections,
    }
end

-- ============================================================================
-- Wire frame broadcasting
-- ============================================================================

--- Send a single frame to every hub-channel subscriber on this hub.
--- The wire protocol backbone: EB and tree_snapshot both call this so
--- the broadcast loop is in one place.
local function broadcast_frame_to_hub(frame)
    local sent = 0
    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions or {}) do
            if sub.channel == "hub" then
                -- Each subscription gets its own copy with subscriptionId
                -- threaded so the browser/TUI can route the frame to the
                -- right hub-channel handler when a peer holds multiple subs.
                local message = { subscriptionId = sub_id }
                for k, v in pairs(frame) do message[k] = v end
                client:send(message)
                sent = sent + 1
            end
        end
    end
    return sent
end

-- Wire up EB to use the broadcast loop. EB.upsert / EB.patch / EB.remove
-- now ship frames straight to every hub-channel subscriber.
EB.set_broadcaster(broadcast_frame_to_hub)

--- Broadcast `ui_tree_snapshot` frames for every hub-channel subscriber.
---
--- Trees are no longer per-client — selection is browser-side. Same tree
--- ships to every subscriber; tree_snapshot dedups globally on
--- `(surface, subpath)`. Per-client `surface_subpaths` still influence
--- which subpath each client renders (via the hub's resolve_subpath in
--- tree_snapshot), so a deep-linked browser still gets its sub-page.
local function broadcast_ui_tree_snapshots()
    local total_sent = 0
    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions or {}) do
            if sub.channel == "hub" then
                local ok, sent = pcall(client.send_ui_tree_snapshots, client, sub_id)
                if ok and type(sent) == "number" then
                    total_sent = total_sent + sent
                elseif not ok then
                    log.warn(string.format(
                        "broadcast_ui_tree_snapshots: %s -> %s failed: %s",
                        client.peer_id:sub(1, 8), sub_id:sub(1, 16), tostring(sent)))
                end
            end
        end
    end
    if total_sent > 0 then
        log.debug(string.format(
            "Broadcast ui_tree_snapshot (%d frame(s))", total_sent))
    end
end

local function broadcast_ui_route_registry()
    local sub_count = 0
    for _, client in pairs(clients) do
        for sub_id, sub in pairs(client.subscriptions or {}) do
            if sub.channel == "hub" then
                sub_count = sub_count + 1
                local ok, err = pcall(client.send_ui_route_registry, client, sub_id)
                if not ok then
                    log.warn(string.format(
                        "broadcast_ui_route_registry: %s -> %s failed: %s",
                        client.peer_id:sub(1, 8), sub_id:sub(1, 16), tostring(err)))
                end
            end
        end
    end
    if sub_count > 0 then
        log.debug(string.format(
            "Broadcast ui_route_registry to %d subscription(s)", sub_count))
    end
end

-- ============================================================================
-- Hook Observers (Lua → Lua)
-- ============================================================================

-- Phase 4a: re-broadcast the route registry whenever a surface is
-- registered/unregistered. Debounced so a plugin that registers many
-- surfaces in a tight loop coalesces into a single emission.
local ROUTE_REGISTRY_DEBOUNCE_SECS = 0.05

hooks.on("surfaces_changed", "broadcast_ui_route_registry", function(_info)
    timer.after_idle("ui_route_registry_broadcast", ROUTE_REGISTRY_DEBOUNCE_SECS, function()
        broadcast_ui_route_registry()
        -- When a surface appears/disappears, the set of layout trees
        -- changes too — force a structural rebroadcast so the new
        -- surface's initial tree reaches existing browsers.
        broadcast_ui_tree_snapshots()
    end)
end)

hooks.on("agent_created", "broadcast_agent_created", function(info)
    if Session.is_system_session(info) then
        return
    end
    local payload = ClientSessionPayload.build(info, Agent.all_info())
    log.info(string.format("Broadcasting entity_upsert(session): %s",
        payload.id or payload.session_uuid or "?"))

    EB.upsert("session", payload)
    -- Workspaces list may have grown — re-snapshot since workspace patches
    -- are not granular enough to capture "this session now belongs here".
    local Hub = require("lib.hub")
    local ok, workspaces = pcall(function() return Hub.get():list_workspaces() end)
    if ok and type(workspaces) == "table" then
        for _, workspace in ipairs(workspaces) do
            if workspace.workspace_id then
                EB.upsert("workspace", workspace)
            end
        end
    end
end)

hooks.on("agent_deleted", "broadcast_agent_deleted", function(agent_id)
    log.info(string.format("Broadcasting entity_remove(session): %s", agent_id or "?"))

    if agent_id then
        timer.cancel("idle:" .. agent_id)
    end

    if agent_id then
        EB.remove("session", agent_id)
    end

    -- Surviving sessions might leave a workspace empty. Re-snapshot the
    -- workspace list so a fully drained workspace disappears from the
    -- client view.
    local Hub = require("lib.hub")
    local ok, workspaces = pcall(function() return Hub.get():list_workspaces() end)
    if ok and type(workspaces) == "table" then
        for _, workspace in ipairs(workspaces) do
            if workspace.workspace_id then
                EB.upsert("workspace", workspace)
            end
        end
    end
end)

-- Global callable by Rust to update per-client focus state.
function _set_pty_focused(session_uuid, peer_id, focused)
    if session_uuid then
        pty_clients.set_focused(session_uuid, peer_id, focused)
    end
end

-- Send synthetic focus-out to sessions that a disconnecting client had focused.
hooks.on("client_disconnected", "unfocus_on_disconnect", function(info)
    local peer_id = info.peer_id
    if not peer_id then return end

    local focused_sessions = pty_clients.get_focused_sessions(peer_id)
    for _, session_uuid in ipairs(focused_sessions) do
        hub.write_pty(session_uuid, "\x1b[O")
        pty_clients.set_focused(session_uuid, peer_id, false)
        log.debug(string.format("Sent synthetic focus-out to %s on client %s disconnect",
            session_uuid:sub(1, 16), peer_id:sub(1, 8)))
    end
end)

-- Enrich raw PTY notifications from Rust with agent state, then re-dispatch.
hooks.on("_pty_notification_raw", "enrich_and_dispatch", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    info.already_notified = agent and agent.notification or false

    info.has_focus = agent and agent.session_uuid
        and pty_clients.is_any_focused(agent.session_uuid) or false

    info.session_uuid = agent and agent.session_uuid or nil

    hooks.notify("pty_notification", info)
end)

-- Send a web push notification when a PTY notification (bell) fires, AND ship
-- a `transient_event` envelope to every hub-channel subscriber so toast
-- handlers fire on the browser and the TUI's notification overlay updates.
hooks.on("pty_notification", "push_notification", function(info)
    if info.has_focus then return end
    if info.already_notified then return end

    local hub_id = hub.server_id()
    local agent = (info.session_uuid and Agent.get(info.session_uuid))

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

    -- Set notification flag — Session:update will emit an entity_patch for
    -- the badge state separately.
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

    -- Wire protocol — transient_event delivers the toast/banner copy.
    -- Web → toast + drop. TUI → notification overlay + drop. Future
    -- transient event types reuse this envelope.
    broadcast_frame_to_hub({
        v = 2,
        type = "transient_event",
        event_type = "pty_notification",
        session_uuid = info.session_uuid,
        title = title,
        body = body,
    })
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

function _on_pty_input(session_uuid)
    if not session_uuid then return false end
    local agent = Agent.get(session_uuid)

    local cleared, any_remaining = clear_session_notification(session_uuid)
    if cleared and agent then
        hooks.notify("pty_input", { session_uuid = session_uuid })
    end
    return any_remaining
end

function _clear_session_notification(session_uuid)
    local _, any_remaining = clear_session_notification(session_uuid)
    return any_remaining
end

local function queue_osc_session_update(session_uuid, fields)
    if not session_uuid or type(fields) ~= "table" then return end

    local agent = Agent.get(session_uuid)
    if not agent then return end

    local pending = pending_osc_session_updates[session_uuid]
    if type(pending) ~= "table" then
        pending = {}
        pending_osc_session_updates[session_uuid] = pending
    end

    local changed = false
    for k, v in pairs(fields) do
        if agent[k] ~= v then
            agent[k] = v
            pending[k] = v
            changed = true
        end
    end
    if not changed then return end

    timer.after_idle("session_osc_update:" .. session_uuid, OSC_SESSION_UPDATE_DEBOUNCE_SECS, function()
        local current = pending_osc_session_updates[session_uuid]
        pending_osc_session_updates[session_uuid] = nil

        local s = Agent.get(session_uuid)
        if not s or type(current) ~= "table" or next(current) == nil then return end

        s:_sync_session_manifest()
        hooks.notify("session_updated", {
            session_uuid = session_uuid,
            source = "osc_debounced",
            fields = current,
        })
    end)
end

hooks.on("pty_title_changed", "update_agent_title", function(info)
    queue_osc_session_update(info.session_uuid, { title = info.title })
end)

hooks.on("pty_cwd_changed", "update_agent_cwd", function(info)
    queue_osc_session_update(info.session_uuid, { cwd = info.cwd })
end)

hooks.on("pty_prompt", "update_agent_prompt", function(info)
    local agent = (info.session_uuid and Agent.get(info.session_uuid))
    if agent then
        agent.last_prompt_mark = info
    end
end)

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

-- 5s matches the v1 TUI window. With Claude's bursty agent output
-- (5-10s pauses between chunks) a 2s threshold made the activity dot
-- blip on for milliseconds and hard to perceive; 5s keeps the indicator
-- lit through normal think-pauses without spuriously latching idle.
local IDLE_THRESHOLD_SECS = 5

hooks.on("pty_output", "idle_activity_reset", function(ctx, _data)
    local uuid = ctx.session_uuid
    if not uuid then return end

    local HostedPreview = require("lib.hosted_preview")
    if HostedPreview.handle_output(ctx, _data) then
        return
    end

    local session = Agent.get(uuid)
    if not session then return end

    if session.is_idle then
        session:update({ is_idle = false })
    end

    timer.after_idle("idle:" .. uuid, IDLE_THRESHOLD_SECS, function()
        local s = Agent.get(uuid)
        if s and not s.is_idle then
            s:update({ is_idle = true })
        end
    end)
end)

-- NOTE: the v1 `broadcast_session_updated` hook (which fanned out
-- agent_list + ui_layout_trees on every Session:update) is GONE. The
-- entity_patch for changed fields ships from `Session:update` itself via
-- EB.patch — see lib/session.lua.

hooks.on("agent_lifecycle", "broadcast_lifecycle", function(info)
    log.debug(string.format("Broadcasting agent_lifecycle: %s -> %s",
        info.agent_id or "?", info.status or "?"))
    if info.agent_id and info.status then
        EB.patch("session", info.agent_id, { status = info.status })
    end
end)

-- Wire protocol B2 fix: when a workspace transitions to closed (fired
-- by lib/session.lua:_sync_workspace_manifest on the final session close),
-- emit an entity_patch so clients see the status change and filter the
-- workspace out of their UI. Pre-fix, closed workspaces kept showing up in
-- the client's byId store forever because nothing wrote status=closed to
-- the wire. No EB.remove — workspace manifests persist on disk (session
-- recovery may re-open them), so the client-side filter on status is the
-- right semantics, not a hard delete.
hooks.on("workspace_closed", "broadcast_workspace_closed", function(info)
    local ws_id = info and info.workspace_id
    if not ws_id then return end
    EB.patch("workspace", ws_id, { status = "closed" })
end)

-- ============================================================================
-- Rust Event Handlers (Rust → Lua)
-- ============================================================================

local _event_subs = {}

_event_subs[#_event_subs + 1] = events.on("connection_code_ready", function(data)
    log.info("Broadcasting entity_upsert(connection_code)")
    local hub_id = hub.server_id and hub.server_id() or nil
    if not hub_id then return end
    local payload = {
        hub_id = hub_id,
        url = data.url,
        qr_ascii = data.qr_ascii,
    }
    last_connection_code = { url = data.url, qr_ascii = data.qr_ascii }
    state.set("connections.last_connection_code", last_connection_code)
    EB.upsert("connection_code", payload)
end)

_event_subs[#_event_subs + 1] = events.on("preview_dns_ready", function(data)
    local HostedPreview = require("lib.hosted_preview")
    HostedPreview.handle_dns_ready(data)
end)

_event_subs[#_event_subs + 1] = events.on("connection_code_error", function(err)
    log.warn(string.format("Connection code error: %s", err or "unknown"))
    local hub_id = hub.server_id and hub.server_id() or nil
    if not hub_id then return end
    -- Persist the error shape FIRST so the `connection_code` entity
    -- registration in hub/init.lua can rehydrate late subscribers from
    -- state. Without this, a browser connecting after the error fires
    -- would get an empty entity_snapshot(connection_code) instead of
    -- seeing the error banner.
    last_connection_code = {
        error = err or "Connection code not available",
    }
    state.set("connections.last_connection_code", last_connection_code)
    EB.upsert("connection_code", {
        hub_id = hub_id,
        error = last_connection_code.error,
    })
end)

_event_subs[#_event_subs + 1] = events.on("hub_recovery_state", function(info)
    local incoming = (type(info) == "table") and info or {}

    for k in pairs(hub_recovery_state) do
        hub_recovery_state[k] = nil
    end
    for k, v in pairs(incoming) do
        hub_recovery_state[k] = v
    end
    hub_recovery_state.state = hub_recovery_state.state or "starting"
    state.set("connections.hub_recovery_state", hub_recovery_state)

    local hub_id = hub.server_id and hub.server_id() or nil
    if not hub_id then return end
    local payload = { hub_id = hub_id }
    for k, v in pairs(hub_recovery_state) do payload[k] = v end
    EB.upsert("hub", payload)
end)

_event_subs[#_event_subs + 1] = events.on("agent_status_changed", function(info)
    log.debug(string.format("agent_status_changed: %s -> %s",
        info.agent_id or "?", info.status or "?"))
    if info.agent_id and info.status then
        EB.patch("session", info.agent_id, { status = info.status })
    end
end)

_event_subs[#_event_subs + 1] = events.on("process_exited", function(data)
    local session_uuid = data.session_uuid
    local exit_code = data.exit_code
    log.info(string.format("Process exited for %s (code=%s)",
        session_uuid or "?", tostring(exit_code)))

    local HostedPreview = require("lib.hosted_preview")
    if HostedPreview.handle_process_exited(data) then
        return
    end

    local agent = (session_uuid and Agent.get(session_uuid))
    if agent then
        agent:update({ status = "exited" })
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
    broadcast_frame_to_hub = broadcast_frame_to_hub,
    broadcast_ui_tree_snapshots = broadcast_ui_tree_snapshots,
    broadcast_ui_route_registry = broadcast_ui_route_registry,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    for _, sub_id in ipairs(_event_subs) do
        events.off(sub_id)
    end
    _event_subs = {}
    for session_uuid in pairs(pending_osc_session_updates) do
        timer.cancel("session_osc_update:" .. session_uuid)
        pending_osc_session_updates[session_uuid] = nil
    end
    hooks.off("agent_created", "broadcast_agent_created")
    hooks.off("agent_deleted", "broadcast_agent_deleted")
    hooks.off("agent_lifecycle", "broadcast_lifecycle")
    hooks.off("_pty_notification_raw", "enrich_and_dispatch")
    hooks.off("pty_notification", "push_notification")
    hooks.off("pty_title_changed", "update_agent_title")
    hooks.off("pty_cwd_changed", "update_agent_cwd")
    hooks.off("pty_output", "idle_activity_reset")
    hooks.off("pty_prompt", "update_agent_prompt")
    hooks.off("pty_cursor_visibility", "update_agent_cursor")
    hooks.off("client_disconnected", "unfocus_on_disconnect")
    hooks.off("surfaces_changed", "broadcast_ui_route_registry")
    hooks.off("workspace_closed", "broadcast_workspace_closed")
    timer.cancel("ui_route_registry_broadcast")
    -- Wire protocol B6 fix: we DO NOT clear the broadcaster here. The
    -- top-level `EB.set_broadcaster(broadcast_frame_to_hub)` on reload
    -- replaces it atomically, and the old closure keeps working in the
    -- meantime (it captures `clients` via state.get, which survives
    -- reload). Clearing to nil opened a mutator-blackout window where a
    -- Session:update during the reload would silently lose its
    -- entity_patch. If the new module fails to load, the old broadcaster
    -- stays live — safer than dropping frames.
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
