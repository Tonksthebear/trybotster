-- Broker restart recovery handler.
--
-- Fires when try_connect_broker() reconnects to an *existing* broker process
-- (as opposed to spawning a fresh one). This happens when the Hub process
-- restarts while the broker is still running and holding PTY file descriptors.
--
-- Single-PTY model: each session has exactly one PTY.
--
-- Flow:
--   1. Receive broker inventory from Rust (liveness authority).
--   2. Match each broker session to its persisted manifest.
--   3. Construct a real Agent/Accessory instance from the manifest.
--   4. The instance registers with HandleCache, replays broker scrollback,
--      and enters the session registry as a first-class session.

local Agent = require("lib.agent")
local Accessory = require("lib.accessory")
local workspace_store = require("lib.workspace_store")

--- Parse ISO 8601 timestamp to epoch seconds.
-- @param value string|number  ISO 8601 string or epoch number
-- @return number epoch seconds (falls back to os.time())
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

--- Recover a session from its manifest as a real Agent or Accessory instance.
--
-- @param record table  One entry from workspace_store.scan_recoverable_sessions()
-- @param broker_session table  One entry from Rust broker inventory
-- @param recovered table  Array to append recovered sessions to
-- @param seen_keys table  Set of agent_keys already processed
local function recover_session(record, broker_session, recovered, seen_keys)
    local sess         = record.manifest
    local session_uuid = record.session_uuid
    local agent_key    = sess.id

    if not agent_key or agent_key == "" or agent_key == "-" then
        log.debug(string.format("[broker] Skipping session %s: missing id", session_uuid))
        return
    end
    if seen_keys[agent_key] then
        log.debug(string.format("[broker] Skipping session %s: %s already processed",
            session_uuid, agent_key))
        return
    end

    local session_id = tonumber(broker_session.session_id)
    if not session_id then
        log.debug(string.format("[broker] Invalid broker session id for %s", session_uuid))
        return
    end

    local dims = (sess.pty_dimensions or {})["0"] or {}
    local rows = dims.rows or 24
    local cols = dims.cols or 80

    -- Create a lightweight PTY handle (no real process, broker owns the PTY)
    local ok, handle = pcall(
        hub.create_ghost_session, session_uuid, session_id, rows, cols
    )
    if not ok or not handle then
        log.warn(string.format("[broker] create_ghost_session failed for %s: %s",
            agent_key, tostring(handle)))
        return
    end

    -- Read workspace name if not in manifest
    local ws_name = sess.workspace_name
    if not ws_name then
        local data_dir = record.data_dir
        local ws_manifest = workspace_store.read_workspace(data_dir, sess.workspace_id)
        ws_name = ws_manifest and ws_manifest.name or nil
    end

    -- Build recovery config from manifest
    local recovery_config = {
        session_uuid      = session_uuid,
        session_type      = sess.session_type or "agent",
        session_name      = sess.session_name,
        agent_key         = agent_key,
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
        broker_session_id = session_id,
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
        log.warn(string.format("[broker] Failed to recover session %s: %s",
            agent_key, tostring(session)))
        return
    end

    seen_keys[agent_key] = true
    recovered[#recovered + 1] = session
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

    local recovered = {}
    local seen_keys = {}
    local manifest_by_uuid = {}

    -- Build manifest index. Liveness comes from broker inventory, not manifest status.
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
            recover_session(record, broker_session, recovered, seen_keys)
        else
            log.debug(string.format(
                "[broker] No manifest for broker session id=%s uuid=%s",
                tostring(broker_session.session_id),
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
            log.warn(string.format("[broker] Failed to broadcast recovered sessions: %s", tostring(err)))
        else
            log.info(string.format("[broker] Recovered %d session(s)", #recovered))
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
