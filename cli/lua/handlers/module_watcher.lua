-- Filesystem watcher for core Lua module hot-reload (hot-reloadable)
--
-- Watches core Lua modules for file changes and automatically reloads them.
-- Plugins are NOT auto-reloaded — use the reload_plugin command or MCP tool
-- for explicit reloads. This prevents partial-save errors and confusing state.

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
    }

    -- Core module watching only — plugins use explicit reload
    local base_path = _G._lua_base_path
    if base_path and fs.exists(base_path) then
        local wid = watch.directory(base_path, watch_opts, on_core_module_change)
        if wid then
            table.insert(watch_state.ids, wid)
            log.debug(string.format("Module watcher: watching core %s", base_path))
        end
    end

    if #watch_state.ids > 0 then
        log.info(string.format("Module watcher: watching %d directories (core only, plugins use explicit reload)", #watch_state.ids))
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
