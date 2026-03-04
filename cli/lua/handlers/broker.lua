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
--   1. Scan worktrees + data_dir/agents/ for context.json files.
--   2. For each surviving session, call hub.create_ghost_session() to create
--      a shadow-screen-only handle.
--   3. Register ghost handle via hub.register_session() so BrokerPtyOutput
--      frames can be routed.
--   4. Replay broker scrollback into ghost handle's shadow screen.

local Agent = require("lib.agent")

--- Derive the agent key from repo and branch_name.
-- @param repo string  e.g. "owner/repo"
-- @param branch_name string
-- @return string agent key
local function derive_agent_key(repo, branch_name)
    local repo_safe = (repo or ""):gsub("/", "-")
    local branch_safe = (branch_name or ""):gsub("/", "-")
    return repo_safe .. "-" .. branch_safe
end

--- Replay a PTY log file into a ghost handle's shadow screen.
-- @param handle    PtySessionHandle  Ghost handle to replay into
-- @param log_path  string            Absolute path to the active log file
-- @param agent_key string            For log messages only
local function replay_tee_log(handle, log_path, agent_key)
    local files_to_replay = {}
    local rotated = log_path .. ".1"
    if fs.exists(rotated) then
        files_to_replay[#files_to_replay + 1] = rotated
    end
    if fs.exists(log_path) then
        files_to_replay[#files_to_replay + 1] = log_path
    end

    for _, path in ipairs(files_to_replay) do
        local ok, content = pcall(fs.read_bytes, path)
        if ok and content and #content > 0 then
            local feed_ok, feed_err = pcall(function() handle:feed_output(content) end)
            if feed_ok then
                log.info(string.format(
                    "[broker] Replayed %d bytes from log %s → %s",
                    #content, path, agent_key))
            else
                log.warn(string.format(
                    "[broker] feed_output failed replaying %s for %s: %s",
                    path, agent_key, tostring(feed_err)))
            end
        else
            log.debug(string.format(
                "[broker] Could not read tee log %s: %s", path, tostring(content)))
        end
    end
end

--- Process a single context.json file and append a ghost_info to the list.
-- @param context_path string  Absolute path to the context.json file
-- @param in_worktree boolean  True for worktree agents, false for main-branch agents
-- @param ghost_infos table    Array to append the resulting ghost_info to
-- @param seen_keys table      Set of agent_keys already processed
local function process_context_file(context_path, in_worktree, ghost_infos, seen_keys)
    local ok, content = pcall(fs.read, context_path)
    if not ok or not content then
        log.debug(string.format("[broker] Could not read %s: %s", context_path, tostring(content)))
        return
    end

    local ok2, ctx = pcall(json.decode, content)
    if not ok2 or not ctx or not ctx.metadata then
        log.debug(string.format("[broker] Skipping %s: no metadata", context_path))
        return
    end

    local meta = ctx.metadata
    local agent_key = derive_agent_key(ctx.repo, ctx.branch_name)
    if agent_key == "-" or agent_key == "" then
        log.debug(string.format("[broker] Skipping %s: could not derive agent_key", context_path))
        return
    end
    if seen_keys[agent_key] then
        log.debug(string.format("[broker] Skipping %s: already processed as %s", context_path, agent_key))
        return
    end
    seen_keys[agent_key] = true

    -- Read session_uuid from context, or generate one for legacy context files
    local session_uuid = ctx.session_uuid
    if not session_uuid then
        session_uuid = string.format("ghost-%d-%08x", os.time(), math.random(0, 0xFFFFFFFF))
    end

    -- Single broker session (no pty_index loop)
    local session_id = tonumber(meta["broker_session_id"])
    -- Legacy fallback: try broker_session_0
    if not session_id then
        session_id = tonumber(meta["broker_session_0"])
    end
    if not session_id then
        log.debug(string.format("[broker] No broker session found for %s", agent_key))
        return
    end

    local rows = tonumber(meta["broker_pty_rows"]) or tonumber(meta["broker_pty_rows_0"]) or 24
    local cols = tonumber(meta["broker_pty_cols"]) or tonumber(meta["broker_pty_cols_0"]) or 80

    local ok3, ghost_handle = pcall(
        hub.create_ghost_session, session_uuid, session_id, rows, cols
    )
    if not ok3 or not ghost_handle then
        log.warn(string.format(
            "[broker] create_ghost_session failed for %s: %s",
            agent_key, tostring(ghost_handle)
        ))
        return
    end

    -- Register ghost handle in HandleCache
    local ok4, display_idx = pcall(hub.register_session, session_uuid, ghost_handle, {
        session_type = ctx.session_type or "agent",
        agent_key = agent_key,
    })
    if not ok4 then
        log.warn(string.format(
            "[broker] register_session failed for %s: %s",
            agent_key, tostring(display_idx)
        ))
        return
    end

    -- Determine restart type and populate shadow screen
    local any_orphaned = false
    local snapshot = hub.get_pty_snapshot_from_broker(session_id)
    if snapshot ~= nil then
        -- Graceful restart
        if #snapshot > 0 then
            local feed_ok, feed_err = pcall(function() ghost_handle:feed_output(snapshot) end)
            if feed_ok then
                log.info(string.format(
                    "[broker] Graceful: replayed %d snapshot bytes → %s",
                    #snapshot, agent_key))
            else
                log.warn(string.format(
                    "[broker] Graceful: feed_output failed for %s: %s",
                    agent_key, tostring(feed_err)))
            end
        else
            log.info(string.format(
                "[broker] Graceful: empty snapshot for %s (no output yet)", agent_key))
        end
    else
        -- Hard restart (orphaned session)
        any_orphaned = true
        local log_path = meta["tee_log_path"] or meta["tee_log_path_0"]
        if log_path and (fs.exists(log_path) or fs.exists(log_path .. ".1")) then
            replay_tee_log(ghost_handle, log_path, agent_key)
        else
            log.warn(string.format(
                "[broker] Hard restart: no tee log for %s — history unavailable",
                agent_key))
            if log_path then
                local events_path = log_path:match("^(.+)/pty%-%d+%.log$")
                if events_path then
                    local event = json.encode({
                        event = "tee_missing",
                        at = os.date("!%Y-%m-%dT%H:%M:%SZ"),
                        agent_key = agent_key,
                    })
                    pcall(fs.append, events_path .. "/events.jsonl", event .. "\n")
                end
            end
        end
    end

    log.info(string.format(
        "[broker] Ghost session registered: %s (uuid=%s, index=%d, in_worktree=%s, orphaned=%s)",
        agent_key, session_uuid:sub(1, 16), display_idx, tostring(in_worktree), tostring(any_orphaned)
    ))

    local wt_path = ctx.worktree_path
    if not wt_path and in_worktree then
        wt_path = context_path:match("^(.+)/%.botster/context%.json$")
    end

    -- Build workspace name from context fields
    local ws_name = nil
    if ctx.repo then
        local issue_num = meta and meta.issue_number or nil
        if issue_num then
            ws_name = ctx.repo .. "#" .. tostring(issue_num)
        elseif ctx.branch_name then
            ws_name = ctx.repo .. ":" .. ctx.branch_name
        end
    end

    local ghost_info = {
        id             = agent_key,
        session_uuid   = session_uuid,
        session_type   = ctx.session_type or "agent",
        session_name   = "agent",
        display_index  = display_idx,
        workspace_id   = nil,
        workspace_name = ws_name,
        display_name   = ctx.branch_name or agent_key,
        title          = nil,
        cwd            = wt_path,
        agent_name     = ctx.agent_name or ctx.profile_name,
        profile_name   = ctx.agent_name or ctx.profile_name,  -- backward compat
        repo           = ctx.repo,
        metadata       = meta,
        branch_name    = ctx.branch_name,
        worktree_path  = wt_path,
        in_worktree    = in_worktree,
        status         = "ghost",
        orphaned       = any_orphaned,
        notification   = false,
        port           = nil,
        created_at     = nil,
    }
    ghost_infos[#ghost_infos + 1] = ghost_info
end

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

    -- Scan worktree agents
    local worktrees = worktree.list()
    for _, wt in ipairs(worktrees) do
        local context_path = wt.path .. "/.botster/context.json"
        if fs.exists(context_path) then
            process_context_file(context_path, true, ghost_infos, seen_keys)
        end
    end

    -- Scan main-branch agents
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local agents_dir = data_dir .. "/.botster/agents"
        if fs.exists(agents_dir) then
            local ok_ls, entries = pcall(fs.list_dir, agents_dir)
            if ok_ls and entries then
                for _, entry_name in ipairs(entries) do
                    local context_path = agents_dir .. "/" .. entry_name .. "/context.json"
                    if fs.exists(context_path) then
                        process_context_file(context_path, false, ghost_infos, seen_keys)
                    end
                end
            else
                log.debug(string.format("[broker] Could not list %s: %s", agents_dir, tostring(entries)))
            end
        end
    end

    -- Scan Central Session Store
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
