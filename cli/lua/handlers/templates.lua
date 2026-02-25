-- Template install/uninstall/list commands (hot-reloadable)
--
-- Registers template:* commands for browser-side template operations.
-- All paths resolved via fs.resolve_safe(config.lua_path(), dest) before any I/O.
-- Data flows browser <-> CLI over E2E encrypted DataChannel. Nothing through Rails.

local commands = require("lib.commands")

-- ============================================================================
-- Helpers
-- ============================================================================

--- Resolve a relative path safely within the appropriate root.
-- "repo" scope: resolves within {repo}/.botster/
-- "device" scope (or nil/default): resolves within ~/.botster/
-- @param relative string The relative dest path from the template
-- @param scope string|nil "repo" or nil/device
-- @return string|nil absolute_path
-- @return string|nil error
local function safe_path(relative, scope)
    if scope == "repo" then
        local repo_root = worktree.repo_root()
        if not repo_root then return nil, "No repo root" end
        return fs.resolve_safe(repo_root .. "/.botster", relative)
    else
        local root = config.data_dir and config.data_dir() or nil
        if not root then return nil, "No data_dir configured" end
        -- Ensure device root exists (may not yet for first-time initialization)
        if not fs.exists(root) then
            fs.mkdir(root)
        end
        return fs.resolve_safe(root, relative)
    end
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
    local scope = command.scope  -- "device" or "repo"

    if not dest or not content then
        respond(client, sub_id, command.request_id, { ok = false, error = "Missing dest or content" })
        return
    end

    local path, err = safe_path(dest, scope)
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
        log.info(string.format("Template installed: %s (scope=%s)", dest, scope or "device"))
        respond(client, sub_id, command.request_id, { ok = true, dest = dest, scope = scope or "device" })
    else
        respond(client, sub_id, command.request_id, { ok = false, error = write_err })
    end
end, { description = "Install a template file" })

commands.register("template:uninstall", function(client, sub_id, command)
    local dest = command.dest
    local scope = command.scope  -- "device" or "repo"

    if not dest then
        respond(client, sub_id, command.request_id, { ok = false, error = "Missing dest" })
        return
    end

    local path, err = safe_path(dest, scope)
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
        log.info(string.format("Template uninstalled: %s (scope=%s)", dest, scope or "device"))
        respond(client, sub_id, command.request_id, { ok = true, dest = dest, scope = scope or "device" })
    else
        respond(client, sub_id, command.request_id, { ok = false, error = del_err })
    end
end, { description = "Uninstall a template file" })

commands.register("template:list", function(client, sub_id, command)
    local ConfigResolver = require("lib.config_resolver")
    local installed = {}

    -- Scan all 4 layers independently so we report every scope a plugin exists in.
    -- (resolve_all merges by name and only keeps the winning layer.)
    local device_root = config.data_dir and config.data_dir() or nil
    local repo_root = worktree.repo_root()
    local active_profile = (config.get and config.get("active_profile")) or nil

    local function scan_layer(base_path, scope_label, path_prefix)
        local plugins = ConfigResolver.read_plugins(base_path)
        for name, plugin in pairs(plugins) do
            local dest = path_prefix .. "plugins/" .. name .. "/init.lua"
            table.insert(installed, { dest = dest, scope = scope_label, name = name })
        end
    end

    -- Device shared
    if device_root and fs.exists(device_root .. "/shared") then
        scan_layer(device_root .. "/shared", "device", "shared/")
    end

    -- Device profiles
    if device_root then
        local profiles = ConfigResolver.list_profiles_all(device_root, nil)
        for _, profile_name in ipairs(profiles) do
            local profile_path = device_root .. "/profiles/" .. profile_name
            if fs.exists(profile_path) then
                scan_layer(profile_path, "device", "profiles/" .. profile_name .. "/")
            end
        end
    end

    -- Repo shared
    if repo_root and fs.exists(repo_root .. "/.botster/shared") then
        scan_layer(repo_root .. "/.botster/shared", "repo", "shared/")
    end

    -- Repo profiles
    if repo_root then
        local profiles = ConfigResolver.list_profiles_all(nil, repo_root)
        for _, profile_name in ipairs(profiles) do
            local profile_path = repo_root .. "/.botster/profiles/" .. profile_name
            if fs.exists(profile_path) then
                scan_layer(profile_path, "repo", "profiles/" .. profile_name .. "/")
            end
        end
    end

    respond(client, sub_id, command.request_id, { ok = true, installed = installed })
end, { description = "List installed templates" })

-- ============================================================================
-- Plugin Reload Commands
-- ============================================================================

local loader = require("hub.loader")

commands.register("plugin:reload", function(client, sub_id, command)
    local name = command.plugin_name
    if not name then
        respond(client, sub_id, command.request_id, { ok = false, error = "Missing plugin_name" })
        return
    end

    log.info(string.format("Plugin reload requested: %s", name))
    local ok, err = loader.reload_plugin(name)
    respond(client, sub_id, command.request_id, { ok = ok, error = err, plugin_name = name })
end, { description = "Reload a plugin by name" })

commands.register("plugin:load", function(client, sub_id, command)
    local name = command.plugin_name
    if not name then
        respond(client, sub_id, command.request_id, { ok = false, error = "Missing plugin_name" })
        return
    end

    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})

    -- Already loaded? Reload instead
    if registry[name] then
        local ok, err = loader.reload_plugin(name)
        respond(client, sub_id, command.request_id, { ok = ok, error = err, plugin_name = name })
        return
    end

    -- Discover from disk
    local ConfigResolver = require("lib.config_resolver")
    local opts = state.get("plugin_resolver_opts", {})
    local unified = ConfigResolver.resolve_all({
        device_root = opts.device_root,
        repo_root = opts.repo_root,
        profile = opts.profile,
        require_agent = false,
    })

    local found = nil
    if unified and unified.plugins then
        for _, plugin in ipairs(unified.plugins) do
            if plugin.name == name then
                found = plugin
                break
            end
        end
    end

    if not found then
        respond(client, sub_id, command.request_id, { ok = false, error = "Plugin not found on disk: " .. name })
        return
    end

    local ok = loader.load_plugin(found.init_path, name)
    if ok then
        registry[name] = { path = found.init_path }
    end
    respond(client, sub_id, command.request_id, { ok = ok, plugin_name = name })
end, { description = "Load a newly installed plugin" })

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
