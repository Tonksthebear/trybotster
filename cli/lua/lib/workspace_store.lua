-- Workspace and session manifest store (Phase 1: Central Session Store)
--
-- Manages the workspace/session directory hierarchy under data_dir/workspaces/.
-- Provides a central session registry that survives worktree deletion and Hub restart.
--
-- Directory layout:
--   data_dir/workspaces/<workspace-id>/manifest.json
--   data_dir/workspaces/<workspace-id>/sessions/<session-uuid>/manifest.json
--   data_dir/workspaces/<workspace-id>/sessions/<session-uuid>/events.jsonl
--
-- All writes go through write_atomic() for crash safety.
-- This module is hot-reloadable. The only module-level state is _id_counter,
-- which is intentional: it prevents two IDs generated in the same wall-clock
-- second from colliding. It resets to 0 on hot-reload, which is safe because
-- the timestamp component still guarantees cross-reload uniqueness.

local M = {}

-- Monotonically incrementing counter used to differentiate IDs generated in
-- the same second. Incremented before each math.randomseed() call.
local _id_counter = 0

-- =============================================================================
-- ID Generation
-- =============================================================================

--- Generate 6 random lowercase hex characters.
-- Seeds math.random with os.time() * large_prime + counter so IDs generated
-- in the same second differ from each other.
-- @return string 6-char hex string
local function rand_hex6()
    _id_counter = _id_counter + 1
    math.randomseed(os.time() * 1000003 + _id_counter)
    local hex = "0123456789abcdef"
    local r = ""
    for _ = 1, 6 do
        local idx = math.random(1, 16)
        r = r .. hex:sub(idx, idx)
    end
    return r
end

--- Generate a unique session UUID.
-- Format: sess-<timestamp_ms>-<6hex>
-- @return string
function M.generate_session_uuid()
    return string.format("sess-%d-%s", os.time() * 1000, rand_hex6())
end

--- Generate a unique workspace ID.
-- Format: ws-<timestamp_ms>-<6hex>
-- @return string
function M.generate_workspace_id()
    return string.format("ws-%d-%s", os.time() * 1000, rand_hex6())
end

-- =============================================================================
-- Path Helpers
-- =============================================================================

--- Absolute path to the workspaces root directory.
-- @param data_dir string
-- @return string
function M.workspaces_dir(data_dir)
    return data_dir .. "/workspaces"
end

--- Absolute path to a workspace directory.
-- @param data_dir string
-- @param workspace_id string
-- @return string
function M.workspace_dir(data_dir, workspace_id)
    return data_dir .. "/workspaces/" .. workspace_id
end

--- Absolute path to a workspace manifest file.
-- @param data_dir string
-- @param workspace_id string
-- @return string
function M.workspace_manifest_path(data_dir, workspace_id)
    return data_dir .. "/workspaces/" .. workspace_id .. "/manifest.json"
end

--- Absolute path to a session directory.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @return string
function M.session_dir(data_dir, workspace_id, session_uuid)
    return data_dir .. "/workspaces/" .. workspace_id .. "/sessions/" .. session_uuid
end

--- Absolute path to a session manifest file.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @return string
function M.session_manifest_path(data_dir, workspace_id, session_uuid)
    return data_dir .. "/workspaces/" .. workspace_id
        .. "/sessions/" .. session_uuid .. "/manifest.json"
end

--- Absolute path to a session events log.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @return string
function M.events_path(data_dir, workspace_id, session_uuid)
    return data_dir .. "/workspaces/" .. workspace_id
        .. "/sessions/" .. session_uuid .. "/events.jsonl"
end

-- =============================================================================
-- Internal I/O Helpers
-- =============================================================================

--- Ensure a directory exists, creating it if needed.
-- @param dir string Absolute directory path
-- @return boolean true on success or already-exists
local function ensure_dir(dir)
    if not fs.exists(dir) then
        local ok, err = pcall(fs.mkdir, dir)
        if not ok then
            log.warn(string.format("[workspace_store] mkdir failed for %s: %s", dir, tostring(err)))
            return false
        end
    end
    return true
end

--- Write content to path atomically: write to <path>.tmp then rename into place.
-- @param path string  Destination path
-- @param content string  File content
-- @return boolean success
local function write_atomic(path, content)
    local tmp = path .. ".tmp"
    local ok, err = pcall(fs.write, tmp, content)
    if not ok then
        log.warn(string.format("[workspace_store] write_atomic: write failed %s: %s", tmp, tostring(err)))
        return false
    end
    local ok2, err2 = pcall(fs.rename, tmp, path)
    if not ok2 then
        log.warn(string.format("[workspace_store] write_atomic: rename failed %s → %s: %s",
            tmp, path, tostring(err2)))
        pcall(fs.delete, tmp)
        return false
    end
    return true
end

--- Append a single line to a file using Lua's standard io.open("a").
-- @param path string  File path (created if absent)
-- @param line string  Line content (newline appended automatically)
local function append_line(path, line)
    local ok, f_or_err = pcall(io.open, path, "a")
    if ok and f_or_err then
        f_or_err:write(line .. "\n")
        f_or_err:close()
    else
        log.warn(string.format("[workspace_store] append_line: could not open %s: %s",
            path, tostring(f_or_err)))
    end
end

-- =============================================================================
-- Public API
-- =============================================================================

--- Ensure the workspaces root directory exists.
-- Call once on hub startup before any manifest writes.
-- @param data_dir string
function M.init_dir(data_dir)
    ensure_dir(M.workspaces_dir(data_dir))
end

--- Write (or update) a workspace manifest.
-- Creates the workspace directory if it does not yet exist.
-- @param data_dir string
-- @param workspace_id string
-- @param manifest table
function M.write_workspace(data_dir, workspace_id, manifest)
    local dir = M.workspace_dir(data_dir, workspace_id)
    if not ensure_dir(dir) then return end
    local ok, content = pcall(json.encode, manifest)
    if not ok then
        log.warn(string.format("[workspace_store] write_workspace: json.encode failed: %s",
            tostring(content)))
        return
    end
    write_atomic(M.workspace_manifest_path(data_dir, workspace_id), content)
end

--- Write (or update) a session manifest.
-- Creates the session directory if it does not yet exist.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @param manifest table
function M.write_session(data_dir, workspace_id, session_uuid, manifest)
    local dir = M.session_dir(data_dir, workspace_id, session_uuid)
    if not ensure_dir(dir) then return end
    local ok, content = pcall(json.encode, manifest)
    if not ok then
        log.warn(string.format("[workspace_store] write_session: json.encode failed: %s",
            tostring(content)))
        return
    end
    write_atomic(M.session_manifest_path(data_dir, workspace_id, session_uuid), content)
end

--- Append an event record to a session's events.jsonl.
-- Creates the events file if absent.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @param event_name string  e.g. "created", "resurrected", "orphaned", "migrated"
function M.append_event(data_dir, workspace_id, session_uuid, event_name)
    ensure_dir(M.session_dir(data_dir, workspace_id, session_uuid))
    local ok, line = pcall(json.encode, {
        event = event_name,
        at    = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()),
    })
    if ok then
        append_line(M.events_path(data_dir, workspace_id, session_uuid), line)
    end
end

--- Read and decode a session manifest.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @return table|nil  manifest table, or nil on error
function M.read_session(data_dir, workspace_id, session_uuid)
    local path = M.session_manifest_path(data_dir, workspace_id, session_uuid)
    local ok, content = pcall(fs.read, path)
    if not ok or not content then return nil end
    local ok2, manifest = pcall(json.decode, content)
    if not ok2 or not manifest then return nil end
    return manifest
end

--- Scan the workspaces directory for sessions with status == "active".
-- Returns an array of records, each with fields:
--   workspace_id, session_uuid, manifest (decoded table), data_dir
-- @param data_dir string
-- @return array
function M.scan_active_sessions(data_dir)
    local results = {}
    local ws_dir = M.workspaces_dir(data_dir)
    if not fs.exists(ws_dir) then return results end

    local ws_entries, ws_err = fs.list_dir(ws_dir)
    if not ws_entries then
        log.debug(string.format("[workspace_store] scan: could not list %s: %s",
            ws_dir, tostring(ws_err)))
        return results
    end

    for _, workspace_id in ipairs(ws_entries) do
        local sessions_dir = M.workspace_dir(data_dir, workspace_id) .. "/sessions"
        if not fs.exists(sessions_dir) then
            goto continue_workspace
        end

        local sess_entries, sess_err = fs.list_dir(sessions_dir)
        if not sess_entries then
            log.debug(string.format("[workspace_store] scan: could not list %s: %s",
                sessions_dir, tostring(sess_err)))
            goto continue_workspace
        end

        for _, session_uuid in ipairs(sess_entries) do
            local manifest = M.read_session(data_dir, workspace_id, session_uuid)
            if manifest and manifest.status == "active" then
                results[#results + 1] = {
                    workspace_id = workspace_id,
                    session_uuid = session_uuid,
                    manifest     = manifest,
                    data_dir     = data_dir,
                }
            end
        end

        ::continue_workspace::
    end

    return results
end

-- =============================================================================
-- Migration
-- =============================================================================

--- Migrate old-style context.json files to the new workspace/session structure.
--
-- Scans:
--   <data_dir>/.botster/agents/*/context.json   (old main-branch agent store)
--   Each linked worktree's <path>/.botster/context.json
--
-- For each file found: generates new IDs, maps fields to the session manifest
-- schema, writes workspace + session manifests, then deletes the old file.
-- Idempotent: already-migrated files are deleted so re-runs are safe.
--
-- @param data_dir string  Device config directory (config.data_dir())
function M.migrate(data_dir)
    local count = 0

    local function migrate_one(context_path, worktree_path)
        local ok, content = pcall(fs.read, context_path)
        if not ok or not content then return end

        local ok2, ctx = pcall(json.decode, content)
        if not ok2 or not ctx or not ctx.repo or not ctx.branch_name then
            log.debug(string.format("[workspace_store] migrate: skipping %s (invalid)", context_path))
            return
        end

        local workspace_id = M.generate_workspace_id()
        local session_uuid = M.generate_session_uuid()
        local meta         = ctx.metadata or {}
        local issue_number = meta.issue_number
        local now          = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())

        -- Workspace manifest
        local ws_title
        if issue_number then
            ws_title = ctx.repo .. " — issue #" .. tostring(issue_number)
        else
            ws_title = ctx.repo .. " — " .. ctx.branch_name
        end

        local workspace_manifest = {
            id           = workspace_id,
            title        = ws_title,
            repo         = ctx.repo,
            issue_number = issue_number,
            status       = "active",
            created_at   = ctx.created_at or now,
            updated_at   = now,
        }

        -- Lift flat broker_session_N / broker_pty_rows_N keys into structured tables
        local broker_sessions = {}
        local pty_dimensions  = {}
        local idx = 0
        while true do
            local sid = meta["broker_session_" .. idx]
            if not sid then break end
            broker_sessions[tostring(idx)] = tonumber(sid)
            local rows = tonumber(meta["broker_pty_rows_" .. idx])
            local cols = tonumber(meta["broker_pty_cols_" .. idx])
            if rows and cols then
                pty_dimensions[tostring(idx)] = { rows = rows, cols = cols }
            end
            idx = idx + 1
        end

        local repo_safe   = (ctx.repo or ""):gsub("/", "-")
        local branch_safe = (ctx.branch_name or ""):gsub("/", "-")
        local agent_key   = repo_safe .. "-" .. branch_safe

        local session_manifest = {
            uuid          = session_uuid,
            workspace_id  = workspace_id,
            agent_key     = agent_key,
            type          = "agent",
            role          = "developer",
            repo          = ctx.repo,
            branch        = ctx.branch_name,
            worktree_path = ctx.worktree_path or worktree_path,
            profile_name  = ctx.profile_name,
            status        = "active",
            broker_sessions = broker_sessions,
            pty_dimensions  = pty_dimensions,
            created_at    = ctx.created_at or now,
            updated_at    = now,
        }

        M.write_workspace(data_dir, workspace_id, workspace_manifest)
        M.write_session(data_dir, workspace_id, session_uuid, session_manifest)
        M.append_event(data_dir, workspace_id, session_uuid, "migrated")

        local ok3, del_err = pcall(fs.delete, context_path)
        if not ok3 then
            log.warn(string.format("[workspace_store] migrate: could not delete %s: %s",
                context_path, tostring(del_err)))
        end

        count = count + 1
        log.info(string.format("[workspace_store] Migrated %s → workspace %s / session %s",
            context_path, workspace_id, session_uuid))
    end

    -- Scan old data_dir agents store
    local agents_dir = data_dir .. "/.botster/agents"
    if fs.exists(agents_dir) then
        local entries, _ = fs.list_dir(agents_dir)
        if entries then
            for _, entry_name in ipairs(entries) do
                local ctx_path = agents_dir .. "/" .. entry_name .. "/context.json"
                if fs.exists(ctx_path) then
                    migrate_one(ctx_path, nil)
                end
            end
        end
    end

    -- Scan linked worktrees
    local wt_ok, wt_list = pcall(worktree.list)
    if wt_ok and wt_list then
        for _, wt in ipairs(wt_list) do
            local ctx_path = wt.path .. "/.botster/context.json"
            if fs.exists(ctx_path) then
                migrate_one(ctx_path, wt.path)
            end
        end
    end

    if count > 0 then
        log.info(string.format(
            "[workspace_store] Migration complete: %d context.json file(s) migrated", count))
    end
end

return M
