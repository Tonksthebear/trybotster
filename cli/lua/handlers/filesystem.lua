-- Filesystem command handlers (hot-reloadable)
--
-- Registers fs:* commands for browser-side file operations.
-- All paths resolved via fs.resolve_safe(repo_root, relative) before any I/O.
-- Data flows browser <-> CLI over E2E encrypted DataChannel. Nothing through Rails.

local commands = require("lib.commands")
local TargetContext = require("lib.target_context")

-- ============================================================================
-- Helpers
-- ============================================================================

--- Resolve a relative path safely within the appropriate root.
-- When scope is "device", resolves within ~/.botster (config.data_dir()).
-- Otherwise (nil or "repo"), resolves within the repo root.
-- @param relative string The relative path from the browser
-- @param scope string|nil "device" or nil/repo
-- @return string|nil absolute_path
-- @return string|nil error
local function resolve_scope_root(scope, command)
    if scope == "device" then
        local root = config.data_dir and config.data_dir() or nil
        if not root then return nil, "No device data_dir configured" end
        if not fs.exists(root) then
            fs.mkdir(root)
        end
        return root, nil
    end

    local target, target_err = TargetContext.resolve({
        command = command,
        metadata = command and command.metadata or nil,
        require_target_id = true,
        require_target_path = true,
    })
    if not target then
        return nil, tostring(target_err)
    end
    return target.target_path, nil
end

local function safe_path(relative, scope, command)
    local root
    local root_err
    root, root_err = resolve_scope_root(scope, command)
    if not root then return nil, root_err end
    return fs.resolve_safe(root, relative)
end

local function normalize_host_directory(path)
    path = tostring(path or "")
    if path == "" then
        path = os.getenv("HOME") or "/"
    end

    if path:find("\0", 1, true) then
        return nil, "Path contains null byte"
    end

    if not path:match("^/") then
        return nil, "Absolute path required"
    end

    local normalized = path:gsub("/+", "/")
    if normalized ~= "/" then
        normalized = normalized:gsub("/+$", "")
    end

    local stat_result, stat_err = fs.stat(normalized)
    if not stat_result then
        return nil, stat_err or "Path does not exist"
    end

    if stat_result.type ~= "directory" then
        return nil, "Path is not a directory"
    end

    return normalized, nil
end

--- Send a response back to the browser client.
-- @param client The Client instance
-- @param sub_id string Subscription ID for routing
-- @param request_id string Correlation ID from the request
-- @param msg_type string Message type (e.g., "fs:read")
-- @param data table Response payload
local function respond(client, sub_id, request_id, msg_type, data)
    data.type = msg_type
    data.request_id = request_id
    data.subscriptionId = sub_id
    client:send(data)
end

-- ============================================================================
-- Command Handlers
-- ============================================================================

commands.register("fs:read", function(client, sub_id, command)
    local path, err = safe_path(command.path or "", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:read", { ok = false, error = err })
        return
    end

    local content, read_err = fs.read(path)
    if content then
        respond(client, sub_id, command.request_id, "fs:read", {
            ok = true,
            content = content,
            size = #content,
        })
    else
        respond(client, sub_id, command.request_id, "fs:read", { ok = false, error = read_err })
    end
end, { description = "Read a file from the repo" })

commands.register("fs:write", function(client, sub_id, command)
    local path, err = safe_path(command.path or "", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:write", { ok = false, error = err })
        return
    end

    local ok, write_err = fs.write(path, command.content or "")
    if ok then
        respond(client, sub_id, command.request_id, "fs:write", { ok = true })
    else
        respond(client, sub_id, command.request_id, "fs:write", { ok = false, error = write_err })
    end
end, { description = "Write a file to the repo" })

commands.register("fs:list", function(client, sub_id, command)
    local path, err = safe_path(command.path or ".", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:list", { ok = false, error = err })
        return
    end

    local entries_raw, list_err = fs.listdir(path)
    if not entries_raw then
        respond(client, sub_id, command.request_id, "fs:list", { ok = false, error = list_err })
        return
    end

    -- Enrich entries with type and size via stat
    local entries = {}
    for _, name in ipairs(entries_raw) do
        local entry_path = path .. "/" .. name
        local stat_result = fs.stat(entry_path)
        table.insert(entries, {
            name = name,
            type = stat_result and stat_result.type or "file",
            size = stat_result and stat_result.size or 0,
        })
    end

    respond(client, sub_id, command.request_id, "fs:list", { ok = true, entries = entries })
end, { description = "List directory entries in the repo" })

commands.register("fs:browse", function(client, sub_id, command)
    local path, err = normalize_host_directory(command.path)
    if not path then
        respond(client, sub_id, command.request_id, "fs:browse", { ok = false, error = err })
        return
    end

    local entries_raw, list_err = fs.listdir(path)
    if not entries_raw then
        respond(client, sub_id, command.request_id, "fs:browse", { ok = false, error = list_err })
        return
    end

    local directories_only = command.directories_only ~= false
    local entries = {}
    for _, name in ipairs(entries_raw) do
        local entry_path = path == "/" and ("/" .. name) or (path .. "/" .. name)
        local stat_result = fs.stat(entry_path)
        local entry_type = stat_result and stat_result.type or "file"
        if (not directories_only) or entry_type == "directory" then
            table.insert(entries, {
                name = name,
                type = entry_type,
                size = stat_result and stat_result.size or 0,
            })
        end
    end

    respond(client, sub_id, command.request_id, "fs:browse", {
        ok = true,
        path = path,
        entries = entries,
    })
end, { description = "Browse host filesystem directories for spawn target discovery" })

commands.register("fs:stat", function(client, sub_id, command)
    local path, err = safe_path(command.path or "", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:stat", { ok = false, error = err })
        return
    end

    local stat_result, stat_err = fs.stat(path)
    if stat_result then
        respond(client, sub_id, command.request_id, "fs:stat", {
            ok = true,
            exists = stat_result.exists,
            file_type = stat_result.type,
            size = stat_result.size,
        })
    else
        respond(client, sub_id, command.request_id, "fs:stat", { ok = false, error = stat_err })
    end
end, { description = "Get file/directory metadata" })

commands.register("fs:delete", function(client, sub_id, command)
    local path, err = safe_path(command.path or "", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:delete", { ok = false, error = err })
        return
    end

    local ok, del_err = fs.delete(path)
    if ok then
        respond(client, sub_id, command.request_id, "fs:delete", { ok = true })
    else
        respond(client, sub_id, command.request_id, "fs:delete", { ok = false, error = del_err })
    end
end, { description = "Delete a file from the repo" })

commands.register("fs:mkdir", function(client, sub_id, command)
    local path, err = safe_path(command.path or "", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:mkdir", { ok = false, error = err })
        return
    end

    local ok, mkdir_err = fs.mkdir(path)
    if ok then
        respond(client, sub_id, command.request_id, "fs:mkdir", { ok = true })
    else
        respond(client, sub_id, command.request_id, "fs:mkdir", { ok = false, error = mkdir_err })
    end
end, { description = "Create a directory in the repo" })

commands.register("fs:rmdir", function(client, sub_id, command)
    local path, err = safe_path(command.path or "", command.scope, command)
    if not path then
        respond(client, sub_id, command.request_id, "fs:rmdir", { ok = false, error = err })
        return
    end

    local ok, rmdir_err = fs.rmdir(path)
    if ok then
        respond(client, sub_id, command.request_id, "fs:rmdir", { ok = true })
    else
        respond(client, sub_id, command.request_id, "fs:rmdir", { ok = false, error = rmdir_err })
    end
end, { description = "Recursively remove a directory from the repo" })

commands.register("fs:rename", function(client, sub_id, command)
    local from_path, from_err = safe_path(command.from_path or "", command.scope, command)
    if not from_path then
        respond(client, sub_id, command.request_id, "fs:rename", { ok = false, error = from_err })
        return
    end

    local to_path, to_err = safe_path(command.to_path or "", command.scope, command)
    if not to_path then
        respond(client, sub_id, command.request_id, "fs:rename", { ok = false, error = to_err })
        return
    end

    local ok, rename_err = fs.rename(from_path, to_path)
    if ok then
        respond(client, sub_id, command.request_id, "fs:rename", { ok = true })
    else
        respond(client, sub_id, command.request_id, "fs:rename", { ok = false, error = rename_err })
    end
end, { description = "Rename/move a file or directory" })

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {}

function M._before_reload()
    log.info("handlers/filesystem.lua reloading")
end

function M._after_reload()
    log.info("handlers/filesystem.lua reloaded")
end

log.info("Filesystem commands registered")

return M
