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
--   1. Receive broker inventory from Rust (liveness authority).
--   2. Enrich with session manifest metadata (workspace, labels, dimensions).
--   3. For each surviving session, call hub.create_ghost_session() to create
--      a shadow-screen-only handle.
--   4. Register ghost handle via hub.register_session() so BrokerPtyOutput
--      frames can be routed.
--   5. Replay broker snapshot bytes into ghost handle's shadow screen.

local Agent = require("lib.agent")
local workspace_store = require("lib.workspace_store")

local function replay_broker_snapshot(session_id, agent_key, ghost_handle)
    local ok_snapshot, snapshot = pcall(hub.get_pty_snapshot_from_broker, session_id)
    if not ok_snapshot then
        log.warn(string.format(
            "[broker] get_pty_snapshot_from_broker failed for %s (session=%s): %s",
            agent_key, tostring(session_id), tostring(snapshot)
        ))
        return false
    end
    if not snapshot or #snapshot == 0 then
        log.debug(string.format(
            "[broker] Empty broker snapshot for %s (session=%s)",
            agent_key, tostring(session_id)
        ))
        return false
    end

    local ok_feed, feed_err = pcall(function() ghost_handle:feed_output(snapshot) end)
    if ok_feed then
        log.info(string.format(
            "[broker] Replayed %d bytes from broker snapshot → %s",
            #snapshot, agent_key
        ))
        return true
    end

    log.warn(string.format(
        "[broker] feed_output failed for %s: %s",
        agent_key, tostring(feed_err)
    ))
    return false
end

--- Process a single Central Session Store session manifest.
-- @param record table  One entry from workspace_store.scan_recoverable_sessions()
-- @param broker_session table  One entry from Rust broker inventory
-- @param ghost_infos table  Array to append results to
-- @param seen_keys table    Set of agent_keys already processed
local function process_session_manifest(record, broker_session, ghost_infos, seen_keys)
    local sess         = record.manifest
    local workspace_id = record.workspace_id
    local session_uuid = record.session_uuid

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

    local pty_dimensions  = sess.pty_dimensions  or {}
    local session_id = tonumber(broker_session.session_id)
    if not session_id then
        log.debug(string.format("[broker] Invalid broker session id for %s", session_uuid))
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
        return
    end

    -- Register ghost handle in HandleCache
    local ok4, reg_index = pcall(hub.register_session, ghost_uuid, ghost_handle, {
        session_type = sess.type or "agent",
        agent_key = agent_key,
        workspace_id = workspace_id,
        broker_session_id = session_id,
    })
    if not ok4 then
        log.warn(string.format(
            "[broker] register_session failed for %s: %s", agent_key, tostring(reg_index)
        ))
        return
    end
    -- Claim the key slot only after successful session registration.
    seen_keys[agent_key] = true

    replay_broker_snapshot(session_id, agent_key, ghost_handle)

    log.info(string.format(
        "[broker] Ghost session from workspace store: %s (uuid=%s, index=%s, ws=%s)",
        agent_key, ghost_uuid:sub(1, 16), tostring(reg_index), workspace_id
    ))

    -- Convert structured broker_sessions to flat metadata for session info.
    local ghost_meta = {}
    ghost_meta["broker_session_id"] = tostring(session_id)
    if dims.rows then ghost_meta["broker_pty_rows"] = tostring(dims.rows) end
    if dims.cols then ghost_meta["broker_pty_cols"] = tostring(dims.cols) end

    -- Read workspace manifest for name
    local data_dir = record.data_dir
    local ws_manifest = workspace_store.read_workspace(data_dir, workspace_id)
    local workspace_name = ws_manifest and ws_manifest.name or nil

    local ghost_info = {
        id             = agent_key,
        session_uuid   = ghost_uuid,
        session_type   = sess.type or "agent",
        session_name   = "agent",
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
        created_at     = os.time(),
    }
    local registered = Agent.register_ghost(ghost_info)
    if not registered then
        log.warn(string.format("[broker] Failed to register ghost info for %s", agent_key))
        return
    end
    ghost_infos[#ghost_infos + 1] = ghost_info
end

local M = {}
local _event_sub = nil

_event_sub = events.on("broker_sessions_recovered", function(data)
    local sessions = (type(data) == "table" and type(data.sessions) == "table")
        and data.sessions or {}

    log.info(string.format(
        "[broker] Recovering from broker inventory (%d live session(s))",
        #sessions
    ))

    local ghost_infos = {}
    local seen_keys = {}
    local manifest_by_uuid = {}

    -- Build manifest index for metadata enrichment.
    -- Liveness comes from broker inventory, not manifest status.
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local ws = require("lib.workspace_store")
        local records = ws.scan_recoverable_sessions(data_dir)
        for _, record in ipairs(records) do
            manifest_by_uuid[record.session_uuid] = record
        end
        log.info(string.format(
            "[broker] Workspace store: %d recoverable manifest(s) indexed",
            #records
        ))
    else
        log.debug("[broker] No data_dir configured; manifest enrichment unavailable")
    end

    for _, broker_session in ipairs(sessions) do
        local session_uuid = broker_session.session_uuid
        local record = session_uuid and manifest_by_uuid[session_uuid] or nil
        if record then
            process_session_manifest(record, broker_session, ghost_infos, seen_keys)
        else
            log.debug(string.format(
                "[broker] No manifest metadata for broker session id=%s uuid=%s",
                tostring(broker_session.session_id),
                tostring(session_uuid)
            ))
        end
    end

    -- Surface ghost agents in the TUI
    if #ghost_infos > 0 then
        local connections = require("handlers.connections")

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
