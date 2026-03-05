-- Broker restart recovery handler.
--
-- Fires when try_connect_broker() reconnects to an *existing* broker process
-- (as opposed to spawning a fresh one). This happens when the Hub process
-- restarts while the broker is still running and holding PTY file descriptors.
--
-- Single-PTY model: each session has exactly one PTY. Ghost resurrection
-- reads a single broker_session_id per manifest (no pty_index loops).
--
-- Flow:
--   1. Scan Central Session Store for active session manifests.
--   2. For each surviving session, call hub.create_ghost_session() to create
--      a shadow-screen-only handle.
--   3. Register ghost handle via hub.register_session() so BrokerPtyOutput
--      frames can be routed.
--   4. Replay broker scrollback into ghost handle's shadow screen.

local Agent = require("lib.agent")

--- Process a single Central Session Store session manifest.
-- @param record table  One entry from workspace_store.scan_active_sessions()
-- @param ghost_infos table  Array to append results to
-- @param seen_keys table    Set of agent_keys already processed
local function process_session_manifest(record, ghost_infos, seen_keys)
    local sess         = record.manifest
    local workspace_id = record.workspace_id
    local session_uuid = record.session_uuid
    local data_dir     = record.data_dir

    local agent_key = sess.agent_key
    if not agent_key or agent_key == "" or agent_key == "-" then
        log.debug(string.format("[broker] Skipping session %s: missing agent_key", session_uuid))
        return
    end
    if seen_keys[agent_key] then
        log.debug(string.format("[broker] Skipping session %s: %s already processed",
            session_uuid, agent_key))
        return
    end

    local ws = require("lib.workspace_store")

    -- Mark suspended before touching the broker
    sess.status     = "suspended"
    sess.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
    ws.write_session(data_dir, workspace_id, session_uuid, sess)
    ws.refresh_workspace_status(data_dir, workspace_id)

    -- Single broker session from the structured manifest
    local broker_sessions = sess.broker_sessions or {}
    local pty_dimensions  = sess.pty_dimensions  or {}

    local session_id = broker_sessions["0"]
    if not session_id then
        log.debug(string.format("[broker] No broker session found for workspace session %s / %s",
            workspace_id, session_uuid))
        sess.status     = "orphaned"
        sess.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
        ws.write_session(data_dir, workspace_id, session_uuid, sess)
        ws.refresh_workspace_status(data_dir, workspace_id)
        ws.append_event(data_dir, workspace_id, session_uuid, "orphaned")
        return
    end

    local dims = pty_dimensions["0"] or {}
    local rows = dims.rows or 24
    local cols = dims.cols or 80

    -- Use the session_uuid from the manifest as the ghost session UUID
    local ghost_uuid = session_uuid

    local ok3, ghost_handle = pcall(
        hub.create_ghost_session, ghost_uuid, session_id, rows, cols
    )
    if not ok3 or not ghost_handle then
        log.warn(string.format(
            "[broker] create_ghost_session failed for %s: %s",
            agent_key, tostring(ghost_handle)
        ))
        sess.status     = "orphaned"
        sess.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
        ws.write_session(data_dir, workspace_id, session_uuid, sess)
        ws.refresh_workspace_status(data_dir, workspace_id)
        ws.append_event(data_dir, workspace_id, session_uuid, "orphaned")
        return
    end

    -- Claim the key slot now
    seen_keys[agent_key] = true

    -- Register ghost handle in HandleCache
    local ok4, display_idx = pcall(hub.register_session, ghost_uuid, ghost_handle, {
        session_type = sess.type or "agent",
        agent_key = agent_key,
        workspace_id = workspace_id,
    })
    if not ok4 then
        log.warn(string.format(
            "[broker] register_session failed for %s: %s", agent_key, tostring(display_idx)
        ))
        sess.status     = "orphaned"
        sess.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
        ws.write_session(data_dir, workspace_id, session_uuid, sess)
        ws.refresh_workspace_status(data_dir, workspace_id)
        ws.append_event(data_dir, workspace_id, session_uuid, "orphaned")
        return
    end

    -- Replay broker scrollback into ghost shadow screen
    local snapshot = hub.get_pty_snapshot_from_broker(session_id)
    if snapshot ~= nil and #snapshot > 0 then
        local ok5, err = pcall(function() ghost_handle:feed_output(snapshot) end)
        if ok5 then
            log.info(string.format(
                "[broker] Replayed %d bytes → %s (workspace store)",
                #snapshot, agent_key))
        else
            log.warn(string.format(
                "[broker] feed_output failed for %s: %s",
                agent_key, tostring(err)))
        end
    end

    -- Session confirmed alive
    sess.status     = "active"
    sess.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
    ws.write_session(data_dir, workspace_id, session_uuid, sess)
    ws.refresh_workspace_status(data_dir, workspace_id)
    ws.append_event(data_dir, workspace_id, session_uuid, "resurrected")

    log.info(string.format(
        "[broker] Ghost session from workspace store: %s (uuid=%s, index=%d, ws=%s)",
        agent_key, ghost_uuid:sub(1, 16), display_idx, workspace_id
    ))

    -- Convert structured broker_sessions to flat metadata for ghost info compat
    local ghost_meta = {}
    ghost_meta["broker_session_id"] = tostring(session_id)
    if dims.rows then ghost_meta["broker_pty_rows"] = tostring(dims.rows) end
    if dims.cols then ghost_meta["broker_pty_cols"] = tostring(dims.cols) end

    -- Read workspace manifest for name
    local ws_manifest = ws.read_workspace(data_dir, workspace_id)
    local workspace_name = ws_manifest and ws_manifest.name or nil

    local ghost_info = {
        id             = agent_key,
        session_uuid   = ghost_uuid,
        session_type   = sess.type or "agent",
        session_name   = "agent",
        display_index  = display_idx,
        workspace_id   = workspace_id,
        workspace_name = workspace_name,
        display_name   = sess.branch or agent_key,
        title          = nil,
        cwd            = sess.worktree_path,
        agent_name     = sess.agent_name or sess.profile_name,
        profile_name   = sess.agent_name or sess.profile_name,  -- backward compat
        repo           = sess.repo,
        metadata       = ghost_meta,
        branch_name    = sess.branch,
        worktree_path  = sess.worktree_path,
        in_worktree    = sess.worktree_path ~= nil,
        status         = "ghost",
        notification   = false,
        port           = nil,
        created_at     = nil,
    }
    ghost_infos[#ghost_infos + 1] = ghost_info
end

local M = {}
local _event_sub = nil

_event_sub = events.on("broker_reconnected", function()
    log.info("[broker] Hub restarted — scanning for surviving agents")

    local ghost_infos = {}
    local seen_keys = {}

    -- Scan Central Session Store for active sessions
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local ws = require("lib.workspace_store")
        local active_sessions = ws.scan_active_sessions(data_dir)
        log.info(string.format("[broker] Workspace store: %d active session(s) found",
            #active_sessions))
        for _, record in ipairs(active_sessions) do
            process_session_manifest(record, ghost_infos, seen_keys)
        end
    else
        log.debug("[broker] No data_dir configured; skipping workspace store scan")
    end

    -- Surface ghost agents in the TUI
    if #ghost_infos > 0 then
        local state = require("hub.state")
        local connections = require("handlers.connections")

        -- Persist ghost infos for late-connecting clients
        local ghost_registry = state.get("ghost_agent_registry", {})
        for _, gi in ipairs(ghost_infos) do
            ghost_registry[gi.id] = gi
        end

        local ok, err = pcall(function()
            for _, ghost_info in ipairs(ghost_infos) do
                hooks.notify("agent_created", ghost_info)
            end
            connections.broadcast_hub_event("agent_list", { agents = Agent.all_info() })
        end)

        if not ok then
            log.warn(string.format("[broker] Failed to broadcast ghost agents: %s", tostring(err)))
        else
            log.info(string.format(
                "[broker] Broadcast %d ghost session(s) to TUI", #ghost_infos
            ))
        end
    end
end)

function M._before_reload()
    if _event_sub then
        events.off(_event_sub)
        _event_sub = nil
    end
end

return M
