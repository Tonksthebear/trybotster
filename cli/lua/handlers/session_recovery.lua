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

local function copy_identity_table(source)
    local out = {}
    if type(source) ~= "table" then return out end
    for k, v in pairs(source) do
        -- Process identity is spawn-time sidecar data, not the mutable
        -- canonical session record. Drop plugin metadata so stale sidecar data
        -- cannot silently rehydrate ownership/workflow state after manifest loss.
        if k ~= "metadata" then out[k] = v end
    end
    return out
end

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

local function build_recovery_config(sess, session_uuid, handle, socket_info, record)
    local dims = (sess.pty_dimensions or {})["0"] or {}
    local rows = socket_info.rows or dims.rows or 24
    local cols = socket_info.cols or dims.cols or 80

    local ws_name = sess.workspace_name
    if not ws_name and record and record.data_dir and sess.workspace_id then
        local ws_manifest = workspace_store.read_workspace(record.data_dir, sess.workspace_id)
        ws_name = ws_manifest and ws_manifest.name or nil
    end

    return {
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
        owner_plugin      = sess.owner_plugin,
        visibility        = sess.visibility,
        surface           = sess.surface,
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
        recovery_source   = sess.recovery_source or "manifest",
        canonical         = sess.canonical ~= false,
        handle            = handle,
        dims              = { rows = rows, cols = cols },
    }
end

local function instantiate_recovered_session(recovery_config)
    if recovery_config.session_type == "accessory" then
        return Accessory.from_recovery(recovery_config)
    end
    return Agent.from_recovery(recovery_config)
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
        log.warn(string.format(
            "[session_recovery] leaving socket in place for inspection: %s",
            tostring(socket_info and socket_info.socket_path or nil)
        ))
        return
    end

    sess.recovery_source = "manifest"
    sess.canonical = true
    local recovery_config = build_recovery_config(sess, session_uuid, handle, socket_info, record)

    -- Construct a real session instance
    local ok2, session = pcall(function()
        return instantiate_recovered_session(recovery_config)
    end)

    if not ok2 or not session then
        log.warn(string.format("[session_recovery] Failed to recover session %s: %s",
            session_uuid, tostring(session)))
        pcall(hub.unregister_session, session_uuid)
        -- Explicitly close the connection so the session process detects
        -- disconnect immediately instead of waiting for Lua GC.
        pcall(handle.kill, handle)
        return
    end

    pcall(function()
        session:_sync_workspace_manifest()
        session:_sync_session_manifest()
    end)

    seen_keys[session_uuid] = true
    recovered[#recovered + 1] = session
end

local function recover_session_from_process_identity(socket_info, recovered, seen_keys)
    local session_uuid = socket_info and socket_info.session_uuid
    if not session_uuid or session_uuid == "" or seen_keys[session_uuid] then return end

    local identity = socket_info.recovery_identity
    if type(identity) ~= "table" then
        log.warn(string.format(
            "[session_recovery] No manifest or process identity for session socket %s",
            tostring(session_uuid)
        ))
        return
    end

    if identity.schema_version ~= 1 then
        log.warn(string.format(
            "[session_recovery] unsupported process identity schema for session %s: %s",
            tostring(session_uuid),
            tostring(identity.schema_version)
        ))
        return
    end

    if tostring(identity.session_uuid or "") ~= session_uuid then
        log.warn(string.format(
            "[session_recovery] process identity UUID mismatch for socket %s: identity=%s",
            tostring(session_uuid),
            tostring(identity.session_uuid)
        ))
        return
    end

    local ok, handle = pcall(
        hub.connect_session, session_uuid, socket_info.socket_path
    )
    if not ok or not handle then
        log.warn(string.format("[session_recovery] identity connect failed for %s: %s",
            tostring(session_uuid), tostring(handle)))
        return
    end

    local sess = copy_identity_table(identity)
    sess.session_uuid = session_uuid
    sess.status = "active"
    sess.recovery_source = "process_identity"
    sess.canonical = false

    local recovery_config = build_recovery_config(sess, session_uuid, handle, socket_info, {
        data_dir = config.data_dir and config.data_dir() or nil,
    })
    local ok2, session = pcall(function()
        return instantiate_recovered_session(recovery_config)
    end)
    if not ok2 or not session then
        log.warn(string.format("[session_recovery] Failed to recover session %s from process identity: %s",
            session_uuid, tostring(session)))
        pcall(hub.unregister_session, session_uuid)
        pcall(handle.kill, handle)
        return
    end

    -- Process identity is self-attested and frozen at spawn. It is enough to
    -- keep a live terminal reachable, but existing manifests remain the
    -- canonical mutable workspace/session record. This intentionally recovers
    -- only a degraded plugin context unless a plugin rehydrates from elsewhere.

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
    local socket_by_uuid = {}

    for _, socket_info in ipairs(sockets) do
        if socket_info.session_uuid and socket_info.session_uuid ~= "" then
            socket_by_uuid[socket_info.session_uuid] = true
        end
    end

    -- Build manifest index from hub manifest's active workspaces.
    -- The hub manifest tracks which workspaces were active — only scan those,
    -- not the entire workspace store. This avoids a full scan of hundreds of
    -- historical workspaces and ignores manifest status (the socket is the
    -- liveness authority, not the status field).
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local ws = require("lib.workspace_store")
        local active_workspaces = {}

        -- Read active workspace IDs from the hub manifest
        local hub_id = hub.hub_id and hub.hub_id() or nil
        if hub_id and hub_discovery and hub_discovery.manifest_path then
            local ok, path = pcall(hub_discovery.manifest_path, hub_id)
            if ok and path then
                local content_ok, content = pcall(fs.read, path)
                if content_ok and content then
                    local json_ok, manifest = pcall(json.decode, content)
                    if json_ok and manifest and manifest.workspaces then
                        active_workspaces = manifest.workspaces
                    end
                end
            end
        end

        local manifest_count = 0
        if #active_workspaces > 0 then
            -- Targeted scan: only look at workspaces the hub had active
            for _, workspace_id in ipairs(active_workspaces) do
                local sessions_dir = ws.workspace_dir(data_dir, workspace_id) .. "/sessions"
                if fs.exists(sessions_dir) then
                    local sess_entries = fs.list_dir(sessions_dir)
                    if sess_entries then
                        for _, session_uuid in ipairs(sess_entries) do
                            local manifest = ws.read_session(data_dir, workspace_id, session_uuid)
                            if manifest and manifest.status ~= "closed" then
                                manifest_by_uuid[session_uuid] = {
                                    workspace_id = workspace_id,
                                    session_uuid = session_uuid,
                                    manifest = manifest,
                                    data_dir = data_dir,
                                }
                                manifest_count = manifest_count + 1
                            end
                        end
                    end
                end
            end
            log.info(string.format(
                "[session_recovery] Scanned %d active workspace(s), found %d session manifest(s)",
                #active_workspaces, manifest_count
            ))
        else
            -- No workspaces in hub manifest — scan recoverable sessions
            local wanted_by_uuid = {}
            for _, socket_info in ipairs(sockets) do
                if socket_info.session_uuid and socket_info.session_uuid ~= "" then
                    wanted_by_uuid[socket_info.session_uuid] = true
                end
            end
            local records = ws.scan_recoverable_sessions(data_dir, wanted_by_uuid)
            for _, record in ipairs(records) do
                manifest_by_uuid[record.session_uuid] = record
                manifest_count = manifest_count + 1
            end
            log.info(string.format(
                "[session_recovery] Scanned workspace store: %d matching recoverable manifest(s)",
                manifest_count
            ))
        end
    end

    for _, socket_info in ipairs(sockets) do
        local session_uuid = socket_info.session_uuid
        local record = session_uuid and manifest_by_uuid[session_uuid] or nil
        if record then
            recover_session(record, socket_info, recovered, seen_keys)
        else
            recover_session_from_process_identity(socket_info, recovered, seen_keys)
        end
    end

    for session_uuid, record in pairs(manifest_by_uuid) do
        local manifest = record and record.manifest or nil
        local status = manifest and manifest.status or nil
        if not socket_by_uuid[session_uuid]
            and (status == "active" or status == "suspended" or status == "running")
        then
            log.warn(string.format(
                "[session_recovery] active manifest has no live session socket; marking orphaned session=%s workspace=%s status=%s",
                tostring(session_uuid),
                tostring(record.workspace_id),
                tostring(status)
            ))
            manifest.status = "orphaned"
            manifest.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
            pcall(function()
                workspace_store.write_session(record.data_dir, record.workspace_id, session_uuid, manifest)
                workspace_store.append_event(record.data_dir, record.workspace_id, session_uuid, "orphaned")
            end)
        end
    end

    -- Broadcast recovered sessions to clients
    if #recovered > 0 then
        local Session = require("lib.session")

        -- Wire protocol: the recovery hook publishes one session entity per
        -- recovered session. No list-style hub fanout is needed here.
        local ok, err = pcall(function()
            for _, session in ipairs(recovered) do
                if not Session.is_system_session(session) then
                    hooks.notify("agent_created", session:info())
                end
            end
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
