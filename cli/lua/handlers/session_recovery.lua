-- Session recovery handler.
--
-- On hub restart, scans the session socket directory for live session processes.
-- Each .sock file represents a session process that survived the restart.
-- The hub connects, handshakes, requests a snapshot, and reconstructs a
-- first-class Agent/Accessory instance.
--
-- Flow:
--   1. Scan socket directory for .sock files (filesystem is the inventory).
--   2. Connect to each socket — if connect fails, remove stale file and skip.
--   3. Handshake provides session_uuid, pid, dimensions, last_output_at.
--   4. Match session_uuid against workspace manifests for enrichment.
--   5. Construct a real Agent/Accessory instance from the manifest.
--   6. Register with HandleCache (installs reader thread automatically).
--   7. Request snapshot from session process to populate shadow screen.

local Agent = require("lib.agent")
local Accessory = require("lib.accessory")
local workspace_store = require("lib.workspace_store")

--- Parse ISO 8601 timestamp to epoch seconds.
local function parse_timestamp(value)
    if type(value) == "number" then return value end
    if type(value) == "string" then
        local y, mo, d, h, mi, s = value:match("(%d+)-(%d+)-(%d+)T(%d+):(%d+):(%d+)")
        if y then
            return os.time({
                year = tonumber(y), month = tonumber(mo), day = tonumber(d),
                hour = tonumber(h), min = tonumber(mi), sec = tonumber(s),
            })
        end
    end
    return os.time()
end

--- Recover a session from its manifest and a live session socket.
local function recover_session(record, socket_info, recovered, seen_keys)
    local sess         = record.manifest
    local session_uuid = record.session_uuid

    if not session_uuid or session_uuid == "" then return end
    if seen_keys[session_uuid] then return end

    -- Connect to the session process socket
    local ok, handle = pcall(
        hub.connect_session, session_uuid, socket_info.socket_path
    )
    if not ok or not handle then
        log.warn(string.format("[session_recovery] connect failed for %s: %s",
            session_uuid, tostring(handle)))
        return
    end

    local dims = (sess.pty_dimensions or {})["0"] or {}
    local rows = socket_info.rows or dims.rows or 24
    local cols = socket_info.cols or dims.cols or 80

    -- Read workspace name
    local ws_name = sess.workspace_name
    if not ws_name then
        local data_dir = record.data_dir
        local ws_manifest = workspace_store.read_workspace(data_dir, sess.workspace_id)
        ws_name = ws_manifest and ws_manifest.name or nil
    end

    -- Build recovery config
    local recovery_config = {
        session_uuid      = session_uuid,
        session_type      = sess.session_type or "agent",
        session_name      = sess.session_name,
        repo              = sess.repo,
        target_id         = sess.target_id,
        target_path       = sess.target_path,
        target_repo       = sess.target_repo,
        branch_name       = sess.branch_name,
        worktree_path     = sess.worktree_path,
        agent_name        = sess.agent_name,
        profile_name      = sess.profile_name,
        metadata          = sess.metadata,
        workspace_id      = sess.workspace_id,
        workspace_name    = ws_name,
        created_at        = parse_timestamp(sess.created_at),
        title             = sess.title,
        cwd               = sess.cwd,
        prompt            = sess.prompt,
        label             = sess.label,
        task              = sess.task,
        in_worktree       = sess.in_worktree,
        handle            = handle,
        dims              = { rows = rows, cols = cols },
    }

    -- Construct a real session instance
    local ok2, session = pcall(function()
        if recovery_config.session_type == "accessory" then
            return Accessory.from_recovery(recovery_config)
        else
            return Agent.from_recovery(recovery_config)
        end
    end)

    if not ok2 or not session then
        log.warn(string.format("[session_recovery] Failed to recover session %s: %s",
            session_uuid, tostring(session)))
        pcall(hub.unregister_session, session_uuid)
        return
    end

    seen_keys[session_uuid] = true
    recovered[#recovered + 1] = session
end

local M = {}
local _event_sub = nil

_event_sub = events.on("sessions_discovered", function(data)
    local sockets = (type(data) == "table" and type(data.sockets) == "table")
        and data.sockets or {}

    log.info(string.format(
        "[session_recovery] Recovering from %d live session socket(s)",
        #sockets
    ))

    local recovered = {}
    local seen_keys = {}
    local manifest_by_uuid = {}

    -- Build manifest index
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local ws = require("lib.workspace_store")
        local records = ws.scan_recoverable_sessions(data_dir)
        for _, record in ipairs(records) do
            manifest_by_uuid[record.session_uuid] = record
        end
        log.info(string.format(
            "[session_recovery] Workspace store: %d recoverable manifest(s) indexed",
            #records
        ))
    end

    for _, socket_info in ipairs(sockets) do
        local session_uuid = socket_info.session_uuid
        local record = session_uuid and manifest_by_uuid[session_uuid] or nil
        if record then
            recover_session(record, socket_info, recovered, seen_keys)
        else
            log.debug(string.format(
                "[session_recovery] No manifest for session socket %s",
                tostring(session_uuid)
            ))
        end
    end

    -- Broadcast recovered sessions to clients
    if #recovered > 0 then
        local connections = require("handlers.connections")

        local ok, err = pcall(function()
            for _, session in ipairs(recovered) do
                hooks.notify("agent_created", session:info())
            end
            connections.broadcast_hub_event("agent_list", { agents = Agent.all_info() })
        end)

        if not ok then
            log.warn(string.format("[session_recovery] Failed to broadcast: %s", tostring(err)))
        else
            log.info(string.format("[session_recovery] Recovered %d session(s)", #recovered))
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
