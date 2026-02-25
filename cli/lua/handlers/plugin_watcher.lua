-- Plugin filesystem watcher (hot-reloadable)
--
-- Watches all plugin directories for file changes and automatically
-- reloads plugins when their files are modified or created.
-- Uses the watch.directory() Rust primitive for OS-level file events.

local loader = require("hub.loader")
local state = require("hub.state")

local M = {}

-- Store watch IDs for cleanup on reload
local watch_state = state.get("plugin_watcher", { ids = {} })

-- Debounce: avoid rapid-fire reloads from editors that write multiple times
local pending_reloads = {}
local DEBOUNCE_MS = 200

--- Extract plugin name from an absolute path.
-- Matches .../plugins/{name}/... and returns name.
-- @param path string Absolute file path
-- @return string|nil Plugin name
local function plugin_name_from_path(path)
    return path:match("/plugins/([^/]+)/")
end

--- Handle a file change event from the watcher.
-- Debounces rapid changes and reloads or loads the plugin.
-- @param event table { path = string, kind = string }
local function on_file_change(event)
    if event.kind ~= "modify" and event.kind ~= "create" then
        return
    end

    local name = plugin_name_from_path(event.path)
    if not name then return end

    -- Debounce: cancel any pending reload for this plugin
    if pending_reloads[name] then
        timer.cancel(pending_reloads[name])
    end

    pending_reloads[name] = timer.once(DEBOUNCE_MS, function()
        pending_reloads[name] = nil

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

--- Set up watches on all plugin directories.
-- Watches the same 4 layers that ConfigResolver scans.
local function setup_watches()
    -- Clean up any existing watches (for reload safety)
    for _, id in ipairs(watch_state.ids) do
        watch.unwatch(id)
    end
    watch_state.ids = {}

    local opts = state.get("plugin_resolver_opts", {})
    local dirs = {}

    -- Device layers
    if opts.device_root then
        table.insert(dirs, opts.device_root .. "/shared/plugins")
        if opts.profile then
            table.insert(dirs, opts.device_root .. "/profiles/" .. opts.profile .. "/plugins")
        end
    end

    -- Repo layers
    if opts.repo_root then
        table.insert(dirs, opts.repo_root .. "/.botster/shared/plugins")
        if opts.profile then
            table.insert(dirs, opts.repo_root .. "/.botster/profiles/" .. opts.profile .. "/plugins")
        end
    end

    for _, dir in ipairs(dirs) do
        if fs.exists(dir) then
            local wid = watch.directory(dir, {
                recursive = true,
                pattern = "*.lua",
            }, on_file_change)

            if wid then
                table.insert(watch_state.ids, wid)
                log.debug(string.format("Plugin watcher: watching %s", dir))
            end
        end
    end

    if #watch_state.ids > 0 then
        log.info(string.format("Plugin watcher: watching %d directories", #watch_state.ids))
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
