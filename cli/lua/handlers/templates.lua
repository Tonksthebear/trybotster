-- Template install/uninstall/list commands (hot-reloadable)
--
-- Registers template:* commands for browser-side template operations.
-- All paths resolved via fs.resolve_safe(config.lua_path(), dest) before any I/O.
-- Data flows browser <-> CLI over E2E encrypted DataChannel. Nothing through Rails.

local commands = require("lib.commands")

-- ============================================================================
-- Helpers
-- ============================================================================

--- Resolve a relative path safely within the Lua config directory.
-- @param relative string The relative dest path from the template
-- @return string|nil absolute_path
-- @return string|nil error
local function safe_path(relative)
    local root = config.lua_path()
    if not root then return nil, "No lua_path configured" end
    return fs.resolve_safe(root, relative)
end

--- Send a response back to the browser client.
-- @param client The Client instance
-- @param sub_id string Subscription ID for routing
-- @param request_id string Correlation ID from the request
-- @param data table Response payload
local function respond(client, sub_id, request_id, data)
    data.type = "template:response"
    data.request_id = request_id
    data.subscriptionId = sub_id
    client:send(data)
end

--- Ensure parent directories exist for a path.
-- fs.mkdir uses create_dir_all internally, so this handles nested paths.
-- @param path string The full file path
-- @return boolean ok
-- @return string|nil error
local function ensure_parent_dirs(path)
    local parent = path:match("^(.+)/[^/]+$")
    if parent then
        return fs.mkdir(parent)
    end
    return true
end

-- ============================================================================
-- Command Handlers
-- ============================================================================

commands.register("template:install", function(client, sub_id, command)
    local dest = command.dest
    local content = command.content

    if not dest or not content then
        respond(client, sub_id, command.request_id, { ok = false, error = "Missing dest or content" })
        return
    end

    local path, err = safe_path(dest)
    if not path then
        respond(client, sub_id, command.request_id, { ok = false, error = err })
        return
    end

    local dir_ok, dir_err = ensure_parent_dirs(path)
    if not dir_ok then
        respond(client, sub_id, command.request_id, { ok = false, error = dir_err or "Failed to create parent directory" })
        return
    end

    local ok, write_err = fs.write(path, content)
    if ok then
        log.info(string.format("Template installed: %s", dest))
        respond(client, sub_id, command.request_id, { ok = true, dest = dest })
    else
        respond(client, sub_id, command.request_id, { ok = false, error = write_err })
    end
end, { description = "Install a template file" })

commands.register("template:uninstall", function(client, sub_id, command)
    local dest = command.dest

    if not dest then
        respond(client, sub_id, command.request_id, { ok = false, error = "Missing dest" })
        return
    end

    local path, err = safe_path(dest)
    if not path then
        respond(client, sub_id, command.request_id, { ok = false, error = err })
        return
    end

    local ok, del_err = fs.delete(path)
    if ok then
        -- Try to remove empty parent directory
        local parent = path:match("^(.+)/[^/]+$")
        if parent then
            local entries = fs.listdir(parent)
            if entries and #entries == 0 then
                fs.rmdir(parent)
            end
        end
        log.info(string.format("Template uninstalled: %s", dest))
        respond(client, sub_id, command.request_id, { ok = true, dest = dest })
    else
        respond(client, sub_id, command.request_id, { ok = false, error = del_err })
    end
end, { description = "Uninstall a template file" })

commands.register("template:list", function(client, sub_id, command)
    local root = config.lua_path()
    if not root then
        respond(client, sub_id, command.request_id, { ok = false, error = "No lua_path configured" })
        return
    end

    local installed = {}

    -- Scan plugins/*/init.lua
    local plugin_dir = root .. "/plugins"
    local plugins = fs.listdir(plugin_dir)
    if plugins then
        for _, name in ipairs(plugins) do
            local init_path = plugin_dir .. "/" .. name .. "/init.lua"
            if fs.exists(init_path) then
                table.insert(installed, "plugins/" .. name .. "/init.lua")
            end
        end
    end

    -- Scan sessions/*/init.lua
    local session_dir = root .. "/sessions"
    local sessions = fs.listdir(session_dir)
    if sessions then
        for _, name in ipairs(sessions) do
            local init_path = session_dir .. "/" .. name .. "/init.lua"
            if fs.exists(init_path) then
                table.insert(installed, "sessions/" .. name .. "/init.lua")
            end
        end
    end

    -- Check user/init.lua
    if fs.exists(root .. "/user/init.lua") then
        table.insert(installed, "user/init.lua")
    end

    respond(client, sub_id, command.request_id, { ok = true, installed = installed })
end, { description = "List installed templates" })

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {}

function M._before_reload()
    log.info("handlers/templates.lua reloading")
end

function M._after_reload()
    log.info("handlers/templates.lua reloaded")
end

log.info("Template commands registered")

return M
