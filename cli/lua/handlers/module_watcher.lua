-- Unified filesystem watcher for hot-reload (hot-reloadable)
--
-- Watches both core Lua modules and plugin directories for file changes
-- and automatically reloads them. Uses the watch.directory() Rust primitive
-- with poll mode for reliable detection on macOS (FSEvents misses in-place writes).
--
-- Core modules: paths under _G._lua_base_path → loader.reload(module_name)
-- Plugins: paths under .botster/*/plugins/ → loader.reload_plugin(name)

local loader = require("hub.loader")
local state = require("hub.state")

local M = {}

-- Store watch IDs for cleanup on reload
local watch_state = state.get("module_watcher", { ids = {} })

-- Debounce: avoid rapid-fire reloads from editors that write multiple times
local pending_reloads = {}
local DEBOUNCE_SECS = 0.2

-- ============================================================================
-- Path Helpers
-- ============================================================================

--- Convert an absolute file path to a Lua module name given a base path.
-- Strips base_path prefix, removes .lua extension, replaces / with .
-- e.g. "/home/user/cli/lua/handlers/agents.lua" with base "/home/user/cli/lua"
--      → "handlers.agents"
-- @param base_path string Base directory
-- @param path string Absolute file path
-- @return string|nil Module name, or nil if path is outside base_path
local function path_to_module(base_path, path)
    -- Ensure base_path ends with /
    local prefix = base_path:gsub("/$", "") .. "/"
    if path:sub(1, #prefix) ~= prefix then
        return nil
    end

    local relative = path:sub(#prefix + 1)
    -- Strip .lua extension
    relative = relative:gsub("%.lua$", "")
    -- Replace / with .
    return relative:gsub("/", ".")
end

--- Extract plugin name from an absolute path.
-- Matches .../plugins/{name}/... and returns name.
-- @param path string Absolute file path
-- @return string|nil Plugin name
local function plugin_name_from_path(path)
    return path:match("/plugins/([^/]+)/")
end

-- ============================================================================
-- Core Module Watching
-- ============================================================================

--- Handle a file change in the core Lua source tree.
local function on_core_module_change(event)
    if event.kind ~= "modify" and event.kind ~= "create" then
        return
    end

    local base_path = _G._lua_base_path
    if not base_path then return end

    local module_name = path_to_module(base_path, event.path)
    if not module_name then return end

    -- Debounce by module name
    local key = "core:" .. module_name
    if pending_reloads[key] then
        timer.cancel(pending_reloads[key])
    end

    pending_reloads[key] = timer.after(DEBOUNCE_SECS, function()
        pending_reloads[key] = nil
        log.info(string.format("Core module changed, reloading: %s", module_name))
        loader.reload(module_name)
    end)
end

-- ============================================================================
-- Plugin Watching
-- ============================================================================

--- Handle a file change in a plugin directory.
local function on_plugin_change(event)
    if event.kind ~= "modify" and event.kind ~= "create" then
        return
    end

    local name = plugin_name_from_path(event.path)
    if not name then return end

    -- Debounce by plugin name
    local key = "plugin:" .. name
    if pending_reloads[key] then
        timer.cancel(pending_reloads[key])
    end

    pending_reloads[key] = timer.after(DEBOUNCE_SECS, function()
        pending_reloads[key] = nil

        local registry = state.get("plugin_registry", {})
        if registry[name] then
            log.info(string.format("Plugin file changed, reloading: %s", name))
            local ok, err = loader.reload_plugin(name)
            if not ok then
                log.error(string.format("Plugin hot-reload failed for %s: %s", name, tostring(err)))
            end
        else
            -- New plugin — check if it has an init.lua
            local init_path = event.path:match("^(.*/plugins/" .. name .. "/init%.lua)$")
            if init_path then
                log.info(string.format("New plugin detected, loading: %s", name))
                local ok = loader.load_plugin(init_path, name)
                if ok then
                    registry[name] = { path = init_path }
                end
            end
        end
    end)
end

-- ============================================================================
-- Watch Setup
-- ============================================================================

local function setup_watches()
    -- Clean up any existing watches (for reload safety)
    for _, id in ipairs(watch_state.ids) do
        watch.unwatch(id)
    end
    watch_state.ids = {}

    local watch_opts = {
        recursive = true,
        pattern = "*.lua",
        poll = true,
    }

    -- Core module watching (source tree or user override dir)
    local base_path = _G._lua_base_path
    if base_path and fs.exists(base_path) then
        local wid = watch.directory(base_path, watch_opts, on_core_module_change)
        if wid then
            table.insert(watch_state.ids, wid)
            log.debug(string.format("Module watcher: watching core %s", base_path))
        end
    end

    -- Plugin directory watching (4 layers from ConfigResolver)
    local opts = state.get("plugin_resolver_opts", {})
    local plugin_dirs = {}

    -- Device layers
    if opts.device_root then
        table.insert(plugin_dirs, opts.device_root .. "/shared/plugins")
        if opts.profile then
            table.insert(plugin_dirs, opts.device_root .. "/profiles/" .. opts.profile .. "/plugins")
        end
    end

    -- Repo layers
    if opts.repo_root then
        table.insert(plugin_dirs, opts.repo_root .. "/.botster/shared/plugins")
        if opts.profile then
            table.insert(plugin_dirs, opts.repo_root .. "/.botster/profiles/" .. opts.profile .. "/plugins")
        end
    end

    for _, dir in ipairs(plugin_dirs) do
        if fs.exists(dir) then
            local wid = watch.directory(dir, watch_opts, on_plugin_change)
            if wid then
                table.insert(watch_state.ids, wid)
                log.debug(string.format("Module watcher: watching plugins %s", dir))
            end
        end
    end

    if #watch_state.ids > 0 then
        log.info(string.format("Module watcher: watching %d directories", #watch_state.ids))
    end
end

setup_watches()

function M._before_reload()
    for _, id in ipairs(watch_state.ids) do
        watch.unwatch(id)
    end
    watch_state.ids = {}
end

function M._after_reload()
    setup_watches()
end

return M
