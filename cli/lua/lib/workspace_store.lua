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
-- Returns true on success, false on any failure.
-- @param data_dir string
-- @param workspace_id string
-- @param manifest table
-- @return boolean success
function M.write_workspace(data_dir, workspace_id, manifest)
    local dir = M.workspace_dir(data_dir, workspace_id)
    if not ensure_dir(dir) then return false end
    local ok, content = pcall(json.encode, manifest)
    if not ok then
        log.warn(string.format("[workspace_store] write_workspace: json.encode failed: %s",
            tostring(content)))
        return false
    end
    return write_atomic(M.workspace_manifest_path(data_dir, workspace_id), content)
end

--- Write (or update) a session manifest.
-- Creates the session directory if it does not yet exist.
-- Returns true on success, false on any failure.
-- @param data_dir string
-- @param workspace_id string
-- @param session_uuid string
-- @param manifest table
-- @return boolean success
function M.write_session(data_dir, workspace_id, session_uuid, manifest)
    local dir = M.session_dir(data_dir, workspace_id, session_uuid)
    if not ensure_dir(dir) then return false end
    local ok, content = pcall(json.encode, manifest)
    if not ok then
        log.warn(string.format("[workspace_store] write_session: json.encode failed: %s",
            tostring(content)))
        return false
    end
    return write_atomic(M.session_manifest_path(data_dir, workspace_id, session_uuid), content)
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

--- Read and decode a workspace manifest.
-- @param data_dir string
-- @param workspace_id string
-- @return table|nil manifest table, or nil on error
function M.read_workspace(data_dir, workspace_id)
    local path = M.workspace_manifest_path(data_dir, workspace_id)
    local ok, content = pcall(fs.read, path)
    if not ok or not content then return nil end
    local ok2, manifest = pcall(json.decode, content)
    if not ok2 or not manifest then return nil end
    return manifest
end

--- Find an existing workspace manifest matching a name.
-- @param data_dir string
-- @param name string  Workspace display name (e.g. "owner/repo#42")
-- @return string|nil workspace_id
-- @return table|nil manifest
function M.find_workspace(data_dir, name)
    if not name or name == "" then return nil, nil end

    local ws_dir = M.workspaces_dir(data_dir)
    if not fs.exists(ws_dir) then return nil, nil end

    local entries, err = fs.list_dir(ws_dir)
    if not entries then
        log.debug(string.format("[workspace_store] find_workspace: could not list %s: %s",
            ws_dir, tostring(err)))
        return nil, nil
    end

    for _, workspace_id in ipairs(entries) do
        local manifest = M.read_workspace(data_dir, workspace_id)
        if manifest and manifest.name == name then
            return workspace_id, manifest
        end
    end

    return nil, nil
end

--- Find or create a workspace manifest for a new session.
-- Callers provide the name (display name); the store matches on it.
-- @param data_dir string
-- @param opts table { name, metadata, created_at }
-- @return string|nil workspace_id
-- @return table|nil manifest
-- @return boolean created_new
function M.ensure_workspace(data_dir, opts)
    local name = opts and opts.name
    if not name or name == "" then
        log.warn("[workspace_store] ensure_workspace: missing name")
        return nil, nil, false
    end

    local existing_id, existing_manifest = M.find_workspace(data_dir, name)
    if existing_id then
        return existing_id, existing_manifest, false
    end

    local workspace_id = M.generate_workspace_id()
    local now = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
    local manifest = {
        id         = workspace_id,
        name       = name,
        status     = "active",
        created_at = opts.created_at or now,
        updated_at = now,
        metadata   = opts.metadata or {},
    }

    local ok = M.write_workspace(data_dir, workspace_id, manifest)
    if not ok then
        return nil, nil, false
    end
    return workspace_id, manifest, true
end

--- Rename a workspace (update the display name).
-- @param data_dir string
-- @param workspace_id string
-- @param new_name string
-- @return boolean success
function M.rename_workspace(data_dir, workspace_id, new_name)
    if not new_name or new_name == "" then
        log.warn("[workspace_store] rename_workspace: empty new_name")
        return false
    end
    local manifest = M.read_workspace(data_dir, workspace_id)
    if not manifest then
        log.warn(string.format("[workspace_store] rename_workspace: workspace %s not found", workspace_id))
        return false
    end
    manifest.name = new_name
    manifest.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
    return M.write_workspace(data_dir, workspace_id, manifest)
end

--- List all workspace manifests.
-- Returns workspace objects sorted by created_at then id.
-- @param data_dir string
-- @return array
function M.list_workspaces(data_dir)
    local out = {}
    local ws_dir = M.workspaces_dir(data_dir)
    if not fs.exists(ws_dir) then return out end

    local entries, err = fs.list_dir(ws_dir)
    if not entries then
        log.debug(string.format("[workspace_store] list_workspaces: could not list %s: %s",
            ws_dir, tostring(err)))
        return out
    end

    for _, workspace_id in ipairs(entries) do
        local manifest = M.read_workspace(data_dir, workspace_id)
        if manifest then
            if not manifest.id or manifest.id == "" then
                manifest.id = workspace_id
            end
            if not manifest.name or manifest.name == "" then
                manifest.name = manifest.id
            end
            local status = M.compute_workspace_status(data_dir, workspace_id)
            if status then
                manifest.status = status
            end
            out[#out + 1] = manifest
        end
    end

    table.sort(out, function(a, b)
        if a.created_at and b.created_at and a.created_at ~= b.created_at then
            return a.created_at < b.created_at
        end
        return tostring(a.id) < tostring(b.id)
    end)

    return out
end

--- Read and decode all session manifests for a workspace.
-- @param data_dir string
-- @param workspace_id string
-- @return array
function M.read_workspace_sessions(data_dir, workspace_id)
    local out = {}
    local sessions_dir = M.workspace_dir(data_dir, workspace_id) .. "/sessions"
    if not fs.exists(sessions_dir) then return out end

    local entries, err = fs.list_dir(sessions_dir)
    if not entries then
        log.debug(string.format("[workspace_store] read_workspace_sessions: could not list %s: %s",
            sessions_dir, tostring(err)))
        return out
    end

    for _, session_uuid in ipairs(entries) do
        local manifest = M.read_session(data_dir, workspace_id, session_uuid)
        if manifest then
            out[#out + 1] = {
                session_uuid = session_uuid,
                manifest = manifest,
            }
        end
    end
    return out
end

--- Derive workspace status from its session statuses.
-- Rules:
--   active     -> any session active
--   orphaned   -> any session orphaned (when none active)
--   closed     -> all closed
--   suspended  -> everything else (including pending/unknown mixes)
-- @param statuses array<string>
-- @return string
function M.derive_workspace_status(statuses)
    local total = 0
    local counts = {
        active = 0,
        suspended = 0,
        orphaned = 0,
        closed = 0,
    }

    for _, status in ipairs(statuses or {}) do
        local s = tostring(status or "")
        total = total + 1
        if counts[s] ~= nil then
            counts[s] = counts[s] + 1
        end
    end

    if total == 0 then return "suspended" end
    if counts.active > 0 then return "active" end
    if counts.orphaned > 0 then return "orphaned" end
    if counts.closed == total then return "closed" end
    return "suspended"
end

--- Compute workspace status from session manifests without writing.
-- @param data_dir string
-- @param workspace_id string
-- @return string|nil status
function M.compute_workspace_status(data_dir, workspace_id)
    local sessions = M.read_workspace_sessions(data_dir, workspace_id)
    local statuses = {}
    for _, rec in ipairs(sessions) do
        statuses[#statuses + 1] = rec.manifest.status
    end
    return M.derive_workspace_status(statuses)
end

--- Recompute and persist one workspace manifest's status from session manifests.
-- @param data_dir string
-- @param workspace_id string
-- @return string|nil status
function M.refresh_workspace_status(data_dir, workspace_id)
    local manifest = M.read_workspace(data_dir, workspace_id)
    if not manifest then return nil end

    local computed = M.compute_workspace_status(data_dir, workspace_id)
    if not computed then return nil end
    if manifest.status == computed then
        return manifest.status
    end

    manifest.status = computed
    manifest.updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time())
    M.write_workspace(data_dir, workspace_id, manifest)
    return manifest.status
end

--- Build grouped workspace payload for agent_list broadcasts.
-- @param data_dir string
-- @param agents array Agent.info()-style tables
-- @return array workspaces
function M.build_workspace_groups(data_dir, agents)
    local grouped = {}
    local by_id = {}
    local agent_list = agents or {}

    for _, agent in ipairs(agent_list) do
        local workspace_id = agent.workspace_id
        if workspace_id then
            if not by_id[workspace_id] then
                local manifest = M.read_workspace(data_dir, workspace_id) or {
                    id = workspace_id,
                    name = agent.workspace_name or (agent.repo or "unknown/repo") .. " — " .. (agent.branch_name or "main"),
                    status = "active",
                    created_at = nil,
                    updated_at = nil,
                    metadata = {},
                }
                local display_name = manifest.name
                if not display_name or display_name == "" then
                    display_name = agent.workspace_name or agent.branch_name or "General"
                end

                -- Read-only status derivation for payloads: avoid write-on-read churn.
                local status = M.compute_workspace_status(data_dir, workspace_id)
                if status then
                    manifest.status = status
                end

                by_id[workspace_id] = {
                    id = workspace_id,
                    name = display_name,
                    status = manifest.status,
                    created_at = manifest.created_at,
                    updated_at = manifest.updated_at,
                    metadata = manifest.metadata or {},
                    agents = {},
                    session_counts = { agent = 0, accessory = 0, other = 0 },
                }
                grouped[#grouped + 1] = by_id[workspace_id]
            end
            by_id[workspace_id].agents[#by_id[workspace_id].agents + 1] = agent.id
            local session_type = tostring(agent.session_type or "agent")
            if session_type == "agent" then
                by_id[workspace_id].session_counts.agent =
                    by_id[workspace_id].session_counts.agent + 1
            elseif session_type == "accessory" then
                by_id[workspace_id].session_counts.accessory =
                    by_id[workspace_id].session_counts.accessory + 1
            else
                by_id[workspace_id].session_counts.other =
                    by_id[workspace_id].session_counts.other + 1
            end
        end
    end

    table.sort(grouped, function(a, b)
        if a.created_at and b.created_at and a.created_at ~= b.created_at then
            return a.created_at < b.created_at
        end
        return tostring(a.id) < tostring(b.id)
    end)

    return grouped
end

--- Scan the workspaces directory for sessions eligible for resurrection.
-- Returns sessions with status == "active" or status == "suspended".
-- "suspended" sessions result from a Hub crash mid-resurrection and must be
-- retried on the next restart; treating them identically to "active" prevents
-- them from becoming permanently unrecoverable.
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
            if manifest and (manifest.status == "active" or manifest.status == "suspended") then
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

--- Scan the workspaces directory for session metadata usable during restart.
--
-- Unlike scan_active_sessions(), this intentionally does NOT decide liveness.
-- Liveness authority is the broker inventory; manifests are metadata-only.
--
-- Returns all session manifests except those explicitly closed.
-- Each record has fields:
--   workspace_id, session_uuid, manifest (decoded table), data_dir
--
-- @param data_dir string
-- @return array
function M.scan_recoverable_sessions(data_dir)
    local results = {}
    local ws_dir = M.workspaces_dir(data_dir)
    if not fs.exists(ws_dir) then return results end

    local ws_entries, ws_err = fs.list_dir(ws_dir)
    if not ws_entries then
        log.debug(string.format("[workspace_store] scan recoverable: could not list %s: %s",
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
            log.debug(string.format("[workspace_store] scan recoverable: could not list %s: %s",
                sessions_dir, tostring(sess_err)))
            goto continue_workspace
        end

        for _, session_uuid in ipairs(sess_entries) do
            local manifest = M.read_session(data_dir, workspace_id, session_uuid)
            if manifest and manifest.status ~= "closed" then
                results[#results + 1] = {
                    workspace_id = workspace_id,
                    session_uuid = session_uuid,
                    manifest = manifest,
                    data_dir = data_dir,
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

        -- Build workspace name from repo + issue/branch
        local ws_name
        local ws_metadata
        if issue_number then
            ws_name = ctx.repo .. "#" .. tostring(issue_number)
        else
            ws_name = ctx.repo .. ":" .. ctx.branch_name
        end
        ws_metadata = { repo = ctx.repo, issue_number = issue_number }

        local workspace_manifest = {
            id         = workspace_id,
            name       = ws_name,
            status     = "active",
            created_at = ctx.created_at or now,
            updated_at = now,
            metadata   = ws_metadata,
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
            agent_name    = ctx.agent_name or ctx.profile_name,
            profile_name  = ctx.agent_name or ctx.profile_name,  -- backward compat
            status        = "active",
            broker_sessions = broker_sessions,
            pty_dimensions  = pty_dimensions,
            created_at    = ctx.created_at or now,
            updated_at    = now,
        }

        local ws_ok   = M.write_workspace(data_dir, workspace_id, workspace_manifest)
        local sess_ok = M.write_session(data_dir, workspace_id, session_uuid, session_manifest)
        if not ws_ok or not sess_ok then
            log.warn(string.format(
                "[workspace_store] migrate: manifest write failed for %s — preserving original",
                context_path))
            return
        end
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

--- Migrate v1 workspace manifests (repo/issue_number/ad_hoc_key) to name format.
-- Scans all workspace manifests. If a manifest has `repo` but no `name`,
-- converts to the new schema. Idempotent: already-converted manifests are skipped.
-- @param data_dir string
function M.migrate_v2(data_dir)
    local ws_dir = M.workspaces_dir(data_dir)
    if not fs.exists(ws_dir) then return end

    local entries, _ = fs.list_dir(ws_dir)
    if not entries then return end

    local count = 0
    for _, workspace_id in ipairs(entries) do
        local manifest = M.read_workspace(data_dir, workspace_id)
        if manifest and not manifest.name and manifest.repo then
            local ws_name
            if manifest.issue_number then
                ws_name = manifest.repo .. "#" .. tostring(manifest.issue_number)
            else
                local branch = manifest.ad_hoc_key or manifest.branch or "main"
                ws_name = manifest.repo .. ":" .. branch
            end
            local ws_metadata = { repo = manifest.repo, issue_number = manifest.issue_number }

            local migrated = {
                id         = manifest.id,
                name       = ws_name,
                status     = manifest.status,
                created_at = manifest.created_at,
                updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()),
                metadata   = ws_metadata,
            }

            M.write_workspace(data_dir, workspace_id, migrated)
            count = count + 1
        end
    end

    if count > 0 then
        log.info(string.format("[workspace_store] migrate_v2: %d workspace(s) converted to name format", count))
    end
end

--- Migrate v2 workspace manifests (dedup_key/title) to v3 (name).
-- Converts dedup_key-based manifests to the new name-based schema.
-- - dedup_key starting "github:" → strip prefix, set as name
-- - dedup_key starting "local:" → delete workspace (per-agent artifacts)
-- - Remove dedup_key and title fields
-- Idempotent: already-converted manifests (those with `name` set) are skipped.
-- @param data_dir string
function M.migrate_v3(data_dir)
    local ws_dir = M.workspaces_dir(data_dir)
    if not fs.exists(ws_dir) then return end

    local entries, _ = fs.list_dir(ws_dir)
    if not entries then return end

    local count = 0
    local deleted = 0
    for _, workspace_id in ipairs(entries) do
        local manifest = M.read_workspace(data_dir, workspace_id)
        if not manifest then goto continue end
        -- Already migrated (has name field)
        if manifest.name then goto continue end
        -- Only migrate manifests that have dedup_key
        if not manifest.dedup_key then goto continue end

        local dk = manifest.dedup_key
        if dk:sub(1, 6) == "local:" then
            -- Per-agent workspace artifacts — delete the entire workspace directory
            -- so scan_active_sessions() does not pick up orphaned session manifests.
            local ws_path = M.workspace_dir(data_dir, workspace_id)
            -- Delete session files first, then session dirs, then workspace manifest/dir
            local sessions_dir = ws_path .. "/sessions"
            if fs.exists(sessions_dir) then
                local sess_entries, _ = fs.list_dir(sessions_dir)
                if sess_entries then
                    for _, sess_id in ipairs(sess_entries) do
                        local sess_path = sessions_dir .. "/" .. sess_id
                        pcall(fs.delete, sess_path .. "/manifest.json")
                        pcall(fs.delete, sess_path .. "/events.jsonl")
                        pcall(fs.rmdir, sess_path)
                    end
                end
                pcall(fs.rmdir, sessions_dir)
            end
            pcall(fs.delete, M.workspace_manifest_path(data_dir, workspace_id))
            pcall(fs.rmdir, ws_path)
            deleted = deleted + 1
            log.info(string.format("[workspace_store] migrate_v3: deleted local workspace %s (%s)",
                workspace_id, dk))
            goto continue
        end

        -- Strip "github:" prefix if present
        local ws_name = dk
        if dk:sub(1, 7) == "github:" then
            ws_name = dk:sub(8)
        end

        local migrated = {
            id         = manifest.id,
            name       = ws_name,
            status     = manifest.status,
            created_at = manifest.created_at,
            updated_at = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()),
            metadata   = manifest.metadata or {},
        }

        M.write_workspace(data_dir, workspace_id, migrated)
        count = count + 1

        ::continue::
    end

    if count > 0 or deleted > 0 then
        log.info(string.format("[workspace_store] migrate_v3: %d workspace(s) converted, %d local workspace(s) deleted",
            count, deleted))
    end
end

return M
