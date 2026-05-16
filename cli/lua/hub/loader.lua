-- Module loader with hot-reload support and trust tiers
--
-- Trust levels:
--   core  - Protected from reload, full access
--   user  - Full access to all primitives (plugins, user/init.lua)
--   agent - Restricted: no process spawn, no keyring, fs limited to improvements/
local M = {}

-- Track which modules should never be reloaded
local protected_modules = {
    ["hub.state"] = true,
    ["hub.hooks"] = true,
    ["hub.loader"] = true,
}

-- Per-plugin log ring buffers: name -> array of {time, level, msg}
local plugin_logs = {}
local MAX_LOG_ENTRIES = 200

-- Reload a module by path
function M.reload(module_name)
    if protected_modules[module_name] then
        log.warn(string.format("Cannot reload protected module: %s", module_name))
        return false
    end

    -- Get the module if already loaded
    local old_module = package.loaded[module_name]

    -- Call _before_reload if it exists
    if old_module and type(old_module) == "table" and old_module._before_reload then
        local ok, err = pcall(old_module._before_reload)
        if not ok then
            log.warn(string.format("_before_reload failed for %s: %s", module_name, tostring(err)))
        end
    end

    -- Unload the module
    package.loaded[module_name] = nil

    -- Reload it
    local ok, result = pcall(require, module_name)
    if not ok then
        log.error(string.format("Failed to reload %s: %s", module_name, tostring(result)))
        -- Restore old module on failure
        package.loaded[module_name] = old_module
        return false
    end

    -- Call _after_reload if it exists
    local new_module = package.loaded[module_name]
    if new_module and type(new_module) == "table" and new_module._after_reload then
        local ok2, err = pcall(new_module._after_reload)
        if not ok2 then
            log.warn(string.format("_after_reload failed for %s: %s", module_name, tostring(err)))
        end
    end

    log.info(string.format("Reloaded module: %s", module_name))
    return true
end

-- Mark a module as protected (cannot be reloaded)
function M.protect(module_name)
    protected_modules[module_name] = true
end

-- Check if a module is protected
function M.is_protected(module_name)
    return protected_modules[module_name] == true
end

-- ============================================================================
-- Plugin package.path helpers
-- ============================================================================

--- Return the directory portion of a file path (equivalent to dirname).
-- @param path string  e.g. "/plugins/github/init.lua"
-- @return string      e.g. "/plugins/github"
local function plugin_dir(path)
    return path:match("^(.*)/[^/]+$") or "."
end

--- Add a plugin directory to package.path (idempotent).
-- Adds both ?.lua and ?/init.lua patterns so plugin authors can choose their
-- own organization next to init.lua or inside lua/.
-- @param dir string  Full path to a plugin-owned directory
local function add_to_package_path(dir)
    local entry1 = dir .. "/?.lua"
    local entry2 = dir .. "/?/init.lua"
    if not package.path:find(entry1, 1, true) then
        package.path = entry1 .. ";" .. entry2 .. ";" .. package.path
    end
end

--- Remove a plugin directory from package.path.
-- Uses split-and-filter to avoid trailing-semicolon edge cases.
-- @param dir string  Full path to a plugin-owned directory
local function remove_from_package_path(dir)
    local entry1 = dir .. "/?.lua"
    local entry2 = dir .. "/?/init.lua"
    local parts = {}
    for part in (package.path .. ";"):gmatch("([^;]*);") do
        if part ~= entry1 and part ~= entry2 and part ~= "" then
            table.insert(parts, part)
        end
    end
    package.path = table.concat(parts, ";")
end

--- Clear package.loaded entries belonging to a plugin namespace.
-- Removes any key equal to `name` or starting with `name.`.
-- @param name string  Plugin name (e.g. "github")
local function clear_plugin_namespace(name)
    local prefix = name .. "."
    for k in pairs(package.loaded) do
        if k == name or k:sub(1, #prefix) == prefix then
            package.loaded[k] = nil
        end
    end
end

local function package_loaded_snapshot()
    local seen = {}
    for k in pairs(package.loaded) do
        seen[k] = true
    end
    return seen
end

local function package_loaded_added_since(before)
    local out = {}
    for k in pairs(package.loaded) do
        if not before[k] then
            out[#out + 1] = k
        end
    end
    table.sort(out)
    return out
end

local function clear_recorded_modules(entry)
    for _, module_name in ipairs(entry.modules or {}) do
        package.loaded[module_name] = nil
    end
end

local function cleanup_plugin_capabilities(key, entry)
    local ok, supervisor = pcall(require, "lib.plugin_supervisor")
    if not ok or type(supervisor) ~= "table" or type(supervisor.cleanup_plugin) ~= "function" then
        return 0
    end
    return supervisor.cleanup_plugin(key, {
        key = key,
        name = entry and entry.name or key,
        plugin_name = entry and entry.name or key,
        path = entry and entry.path or nil,
        source = entry and entry.source or nil,
        repo_root = entry and entry.repo_root or nil,
    })
end

local function load_plugin_worker(key, entry)
    if rawget(_G, "_loading_plugin_worker") == true then return true end
    local ok, supervisor = pcall(require, "lib.plugin_supervisor")
    if not ok or type(supervisor) ~= "table" or type(supervisor.load_plugin) ~= "function" then
        return true
    end
    return supervisor.load_plugin(key, {
        key = key,
        name = entry and entry.name or key,
        plugin_name = entry and entry.name or key,
        path = entry and entry.path or nil,
        source = entry and entry.source or nil,
        repo_root = entry and entry.repo_root or nil,
    })
end

local function shutdown_plugin_worker(key, reason)
    local ok, supervisor = pcall(require, "lib.plugin_supervisor")
    if ok and type(supervisor) == "table" and type(supervisor.shutdown_plugin) == "function" then
        supervisor.shutdown_plugin(key, reason)
    end
end

-- ============================================================================
-- Plugin Status, Logging, and Disabled Set (local helpers — must precede load_plugin)
-- ============================================================================

--- Update a plugin's registry entry with status information.
-- @param name string Plugin name
-- @param status string "loaded" | "errored" | "disabled" | "unloaded"
-- @param error_msg string|nil Error message (for "errored" status)
local function plugin_key(name, opts)
    opts = opts or {}
    if opts.source == "repo" and opts.repo_root and opts.repo_root ~= "" then
        return "repo:" .. tostring(opts.repo_root) .. ":" .. name
    end
    return name
end

function M.plugin_key(name, opts)
    return plugin_key(name, opts)
end

local function update_registry_status(name, status, error_msg)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    local entry = registry[name]
    if not entry then return end

    entry.status = status
    if status == "loaded" then
        entry.loaded_at = os.time()
        entry.error = nil
        entry.error_at = nil
        entry.reload_count = (entry.reload_count or 0) + 1
    elseif status == "errored" then
        entry.error = error_msg
        entry.error_at = os.time()
    elseif status == "disabled" then
        entry.error = nil
    end

    hooks.notify("plugin_status_changed", { name = name, status = status, error = error_msg })
end

local function notify_plugin_reloaded(key, entry)
    if type(hooks) ~= "table" or type(hooks.notify) ~= "function" then
        return
    end
    hooks.notify("plugin_reloaded", {
        key = key,
        name = entry and (entry.name or key) or key,
        path = entry and entry.path or nil,
        source = entry and entry.source or nil,
        repo_root = entry and entry.repo_root or nil,
        reload_count = entry and entry.reload_count or nil,
    })
end

local function upsert_registry_entry(name, path, fields)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    local entry = registry[name] or {
        reload_count = 0,
    }
    entry.key = name
    entry.path = path
    entry.status = entry.status or "pending"
    if fields then
        for k, v in pairs(fields) do
            entry[k] = v
        end
    end
    registry[name] = entry
    return entry
end

local function repo_roots_from_spawn_targets()
    local roots = {}
    local registry = rawget(_G, "spawn_targets")
    if not registry or type(registry.list) ~= "function" then
        return roots
    end

    local ok, targets = pcall(registry.list)
    if not ok or type(targets) ~= "table" then
        return roots
    end

    for _, target in ipairs(targets) do
        if type(target) == "table" and target.enabled ~= false and target.path then
            roots[#roots + 1] = target.path
        end
    end
    return roots
end

local function append_unique(list, seen, value)
    if value and not seen[value] then
        seen[value] = true
        list[#list + 1] = value
    end
end

local function find_plugin_on_disk(name, opts)
    opts = opts or {}
    local ConfigResolver = require("lib.config_resolver")
    local state = require("hub.state")
    local resolver_opts = state.get("plugin_resolver_opts", {})
    local device_root = opts.device_root or resolver_opts.device_root or (config.data_dir and config.data_dir()) or nil

    local repo_roots = {}
    local seen = {}
    append_unique(repo_roots, seen, opts.repo_root)
    for _, repo_root in ipairs(resolver_opts.repo_roots or {}) do
        append_unique(repo_roots, seen, repo_root)
    end
    if opts.include_spawn_targets ~= false then
        for _, repo_root in ipairs(repo_roots_from_spawn_targets()) do
            append_unique(repo_roots, seen, repo_root)
        end
    end

    if device_root then
        local unified = ConfigResolver.resolve_all({
            device_root = device_root,
            repo_root = nil,
            require_agent = false,
        })
        if unified and unified.plugins then
            for _, plugin in ipairs(unified.plugins) do
                if plugin.name == name then
                    plugin.repo_root = nil
                    return plugin
                end
            end
        end
    end

    for _, repo_root in ipairs(repo_roots) do
        local unified = ConfigResolver.resolve_all({
            device_root = device_root,
            repo_root = repo_root,
            require_agent = false,
        })
        if unified and unified.plugins then
            for _, plugin in ipairs(unified.plugins) do
                if plugin.name == name then
                    plugin.repo_root = plugin.source == "repo" and repo_root or nil
                    return plugin
                end
            end
        end
    end

    return nil
end

--- Capture a log entry into a plugin's ring buffer.
-- @param name string Plugin name
-- @param level string Log level
-- @param msg string Log message
local function capture_plugin_log(name, level, msg)
    if not plugin_logs[name] then
        plugin_logs[name] = {}
    end
    local ring = plugin_logs[name]
    table.insert(ring, { time = os.time(), level = level, msg = msg })
    if #ring > MAX_LOG_ENTRIES then
        table.remove(ring, 1)
    end
end

--- Create a wrapped log table that captures entries for a plugin.
-- @param name string Plugin name
-- @param real_log table The real log global
-- @return table Wrapped log table
local function create_plugin_logger(name, real_log)
    if not plugin_logs[name] then
        plugin_logs[name] = {}
    end
    local ring = plugin_logs[name]

    local function capture(level, msg)
        table.insert(ring, { time = os.time(), level = level, msg = msg })
        if #ring > MAX_LOG_ENTRIES then
            table.remove(ring, 1)
        end
    end

    return {
        info = function(msg)
            capture("info", msg)
            real_log.info(string.format("[%s] %s", name, msg))
        end,
        warn = function(msg)
            capture("warn", msg)
            real_log.warn(string.format("[%s] %s", name, msg))
        end,
        error = function(msg)
            capture("error", msg)
            real_log.error(string.format("[%s] %s", name, msg))
        end,
        debug = function(msg)
            capture("debug", msg)
            real_log.debug(string.format("[%s] %s", name, msg))
        end,
    }
end

local function scoped_hook_name(key, name)
    if type(name) ~= "string" then return name end
    local prefix = key .. "::"
    if name:sub(1, #prefix) == prefix then
        return name
    end
    return prefix .. name
end

local function create_scoped_hooks(key, real_hooks)
    if type(real_hooks) ~= "table" then return real_hooks end
    local scoped = {}
    setmetatable(scoped, { __index = real_hooks })

    if type(real_hooks.on) == "function" then
        scoped.on = function(event, name, callback, opts)
            return real_hooks.on(event, scoped_hook_name(key, name), callback, opts)
        end
    end
    if type(real_hooks.off) == "function" then
        scoped.off = function(event, name)
            return real_hooks.off(event, scoped_hook_name(key, name))
        end
    end
    if type(real_hooks.intercept) == "function" then
        scoped.intercept = function(event, name, callback, opts)
            return real_hooks.intercept(event, scoped_hook_name(key, name), callback, opts)
        end
    end
    if type(real_hooks.unintercept) == "function" then
        scoped.unintercept = function(event, name)
            return real_hooks.unintercept(event, scoped_hook_name(key, name))
        end
    end
    if type(real_hooks.enable) == "function" then
        scoped.enable = function(event, name)
            return real_hooks.enable(event, scoped_hook_name(key, name))
        end
    end
    if type(real_hooks.disable) == "function" then
        scoped.disable = function(event, name)
            return real_hooks.disable(event, scoped_hook_name(key, name))
        end
    end

    return scoped
end

--- Persist the disabled set to disk.
local function save_disabled_set()
    local data_dir = config.data_dir and config.data_dir() or nil
    if not data_dir then return end

    local S = M.get_disabled_set()
    local names = {}
    for k, v in pairs(S) do
        if v == true and k ~= "_loaded" then
            table.insert(names, k)
        end
    end
    table.sort(names)
    json.file_set(data_dir .. "/plugin_state.json", "disabled", names)
end

--- Get the disabled plugins set, loading from disk if needed.
-- @return table Set of disabled plugin names
function M.get_disabled_set()
    local state = require("hub.state")
    local S = state.get("plugin_disabled", {})
    -- Lazy-load from disk on first access
    if not S._loaded then
        S._loaded = true
        local data_dir = config.data_dir and config.data_dir() or nil
        if data_dir then
            local path = data_dir .. "/plugin_state.json"
            local disabled, _ = json.file_get(path, "disabled")
            if disabled and type(disabled) == "table" then
                for _, name in ipairs(disabled) do
                    S[name] = true
                end
            end
        end
    end
    return S
end

--- Check if a plugin is disabled.
-- @param name string Plugin name
-- @return boolean
function M.is_disabled(name)
    local S = M.get_disabled_set()
    return S[name] == true
end

-- ============================================================================
-- Plugin Loading
-- ============================================================================

--- Load a plugin by absolute path (not via require/package.path).
-- Loads the file with full _ENV (same trust as user plugins), registers
-- it in package.loaded so it can be reloaded by name.
-- If the plugin directory contains a lua/ subdir, it is added to package.path
-- so the plugin can require() its own modules (e.g. require("telegram.api")).
-- @param path string Absolute path to the plugin's init.lua
-- @param name string Plugin name (used for registration and logging)
-- @param opts table|nil { source = "device"|"repo", repo_root = string }
-- @return boolean success
-- @return string|nil error message on failure
function M.load_plugin(path, name, opts)
    opts = opts or {}
    local key = plugin_key(name, opts)
    upsert_registry_entry(key, path, {
        name = name,
        source = opts.source,
        repo_root = opts.repo_root,
    })

    -- Skip disabled plugins
    if M.is_disabled(key) or (key ~= name and M.is_disabled(name)) then
        log.info(string.format("Skipping disabled plugin: %s", key))
        return false, "Plugin is disabled: " .. key
    end

    if not fs.exists(path) then
        local msg = string.format("load_plugin: %s not found at %s", key, path)
        log.warn(msg)
        return false, msg
    end

    local source, read_err = fs.read(path)
    if not source then
        local msg = string.format("load_plugin: cannot read %s: %s", path, tostring(read_err))
        log.error(msg)
        return false, msg
    end

    local chunk, err = load(source, "@" .. path)
    if not chunk then
        local msg = string.format("load_plugin: syntax error in %s: %s", path, tostring(err))
        capture_plugin_log(key, "error", msg)
        log.error(msg)
        return false, msg
    end

    -- Add plugin-owned directories to package.path before executing the chunk
    -- so require() calls inside init.lua resolve at init time. The plugin root
    -- is included so authors can keep files like web_layout.lua next to
    -- init.lua; lua/ remains supported for larger plugin trees.
    local root_dir = plugin_dir(path)
    add_to_package_path(root_dir)
    local lua_dir = root_dir .. "/lua"
    if fs.is_dir(lua_dir) then
        add_to_package_path(lua_dir)
        log.info(string.format("Plugin %s: registered lua/ at %s", name, lua_dir))
    end

    -- Batch MCP notifications so N mcp.tool()/mcp.prompt() calls emit at most
    -- one notification each instead of N. end_batch() always runs (via pcall)
    -- so batch mode is never left stuck on load error.
    if mcp then mcp.begin_batch() end

    -- Set source context so mcp.tool() can track which plugin registered each tool
    _G._loading_plugin_source = "@" .. path
    _G._loading_plugin_name = key
    _G._loading_plugin_display_name = name
    _G._loading_plugin_key = key
    _G._loading_plugin_repo_root = opts.repo_root or _G._loading_plugin_repo_root

    -- Install per-plugin logger during load
    local real_log = _G.log
    local real_hooks_module = package.loaded["hub.hooks"] or require("hub.hooks")
    local real_hooks = _G.hooks or real_hooks_module
    local scoped_hooks = create_scoped_hooks(key, real_hooks)
    _G.log = create_plugin_logger(key, real_log)
    _G.hooks = scoped_hooks
    if real_hooks_module then
        package.loaded["hub.hooks"] = scoped_hooks
    end

    local loaded_before = package_loaded_snapshot()
    local ok, result = pcall(chunk)

    -- Restore real globals
    _G.log = real_log
    _G.hooks = real_hooks
    package.loaded["hub.hooks"] = real_hooks_module
    _G._loading_plugin_source = nil
    _G._loading_plugin_name = nil
    _G._loading_plugin_display_name = nil
    _G._loading_plugin_key = nil
    if opts.repo_root then
        _G._loading_plugin_repo_root = nil
    end

    if mcp then mcp.end_batch() end

    if not ok then
        local msg = string.format("load_plugin: runtime error in %s: %s", path, tostring(result))
        -- Capture error in plugin's log ring even though logger is restored
        capture_plugin_log(key, "error", msg)
        log.error(msg)
        return false, msg
    end

    -- Register in package.loaded so reload works
    local module_key = "plugin." .. key
    package.loaded[module_key] = result or true
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    if registry[key] then
        registry[key].modules = package_loaded_added_since(loaded_before)
    end
    local worker_ok, worker_err = load_plugin_worker(key, registry[key] or {
        key = key,
        name = name,
        path = path,
        source = opts.source,
        repo_root = opts.repo_root,
    })
    if not worker_ok then
        cleanup_plugin_capabilities(key, registry[key])
        local msg = string.format("load_plugin: worker error in %s: %s", path, tostring(worker_err))
        capture_plugin_log(key, "error", msg)
        log.error(msg)
        return false, msg
    end
    log.info(string.format("Loaded plugin: %s from %s", key, path))
    return true
end

--- Reload a plugin by key using the runtime registry.
-- Plugins are loaded from absolute paths (not package.path), so the standard
-- reload() won't work. This looks up the path from hub.state, runs lifecycle
-- hooks, and re-executes the plugin file.
-- @param name string Plugin name (e.g., "github")
-- @return boolean success
-- @return string|nil error message on failure
function M.reload_plugin(name)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    local entry = registry[name]
    if not entry then
        local discovered = find_plugin_on_disk(name)
        if not discovered then
            return false, "Plugin not found in registry or on disk: " .. name
        end

        local discovered_key = plugin_key(discovered.name or name, {
            source = discovered.source,
            repo_root = discovered.repo_root,
        })
        entry = upsert_registry_entry(discovered_key, discovered.init_path, {
            name = discovered.name or name,
            status = "pending",
            source = discovered.source,
            repo_root = discovered.repo_root,
        })

        local ok, err = M.load_plugin(entry.path, entry.name or name, {
            source = entry.source,
            repo_root = entry.repo_root,
        })
        if ok then
            update_registry_status(discovered_key, "loaded", nil)
            notify_plugin_reloaded(discovered_key, registry[discovered_key] or entry)
        else
            update_registry_status(discovered_key, "errored", err)
        end
        return ok, err
    end

    if M.is_disabled(name) then
        return false, "Plugin is disabled: " .. name .. " (enable it first)"
    end

    local plugin_name = entry.name or name
    local module_key = "plugin." .. (entry.key or name)
    local old = package.loaded[module_key]

    -- Snapshot sub-module cache so we can fully restore it on failure.
    -- If the new plugin partially executes before erroring, it may load some
    -- sub-modules into package.loaded["name.*"]. Without a snapshot the old
    -- module would subsequently require() those new (possibly incompatible)
    -- versions rather than its own originals.
    local old_namespace = {}
    local ns_prefix = plugin_name .. "."
    for k, v in pairs(package.loaded) do
        if k == plugin_name or k:sub(1, #ns_prefix) == ns_prefix then
            old_namespace[k] = v
        end
    end

    -- Lifecycle: cleanup before reload
    if old and type(old) == "table" and old._before_reload then
        local ok, err = pcall(old._before_reload)
        if not ok then
            log.warn(string.format("_before_reload failed for plugin %s: %s", name, tostring(err)))
        end
    end
    cleanup_plugin_capabilities(entry.key or name, entry)
    shutdown_plugin_worker(entry.key or name, "reload")

    -- Batch MCP notifications: suppress mcp_tools_changed/mcp_prompts_changed during
    -- reset + re-registration, then emit exactly once per changed registry at the end.
    -- end_batch() runs even on load failure (registrations were cleared by reset,
    -- clients need one notification to reflect that).
    if mcp then mcp.begin_batch() end

    -- Clear MCP tools registered by this plugin (source = "@" .. path)
    if mcp then
        mcp.reset("@" .. entry.path)
    end

    -- Remove the old package.path entry and sub-module cache so the fresh load
    -- starts clean. load_plugin() re-adds the path after a successful load.
    local root_dir = plugin_dir(entry.path)
    local lua_dir = root_dir .. "/lua"
    remove_from_package_path(root_dir)
    remove_from_package_path(lua_dir)
    clear_recorded_modules(entry)
    clear_plugin_namespace(plugin_name)

    -- Clear old module
    package.loaded[module_key] = nil

    -- Re-load from disk (errors caught internally — load_plugin never throws)
    local ok = M.load_plugin(entry.path, plugin_name, {
        source = entry.source,
        repo_root = entry.repo_root,
    })

    -- Single notification for the entire reload cycle
    if mcp then mcp.end_batch() end

    if not ok then
        -- Full rollback: restore the old module, its sub-module cache, and its
        -- package.path entry so the still-running old module is unaffected by
        -- the failed reload attempt.
        package.loaded[module_key] = old
        for k, v in pairs(old_namespace) do
            package.loaded[k] = v
        end
        add_to_package_path(root_dir)
        if fs.is_dir(lua_dir) then
            add_to_package_path(lua_dir)
        end
        update_registry_status(name, "errored", "Failed to reload plugin: " .. name)
        return false, "Failed to reload plugin: " .. name
    end

    update_registry_status(name, "loaded", nil)
    notify_plugin_reloaded(name, registry[name] or entry)
    return true
end

--- Unload a plugin by name, cleaning up package.path and loaded modules.
--
-- Runs the plugin's `_before_unload` lifecycle hook (if defined), clears MCP
-- tools/prompts registered by this plugin, removes the plugin's lua/ dir from
-- package.path, clears its namespace from package.loaded, and removes it from
-- the plugin registry.
--
-- This is the counterpart to `load_plugin` — call it when a plugin directory
-- is removed so stale MCP registrations and lifecycle hooks don't linger.
--
-- @param name string Plugin name (e.g., "github")
-- @return boolean success
-- @return string|nil error message on failure
function M.unload_plugin(name)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    local entry = registry[name]
    if not entry then
        return false, "Plugin not found in registry: " .. name
    end

    local plugin_name = entry.name or name
    local module_key = "plugin." .. (entry.key or name)
    local mod = package.loaded[module_key]

    -- Notify subscribers (e.g. lib.plugin_db) that the plugin is about to be
    -- torn down so they can release resources keyed by plugin name. Fires
    -- BEFORE the plugin's own `_before_unload` so shared infra runs first.
    hooks.notify("plugin_unloading", { name = name, plugin_name = plugin_name, key = entry.key or name })

    -- Lifecycle: let the plugin clean up before being removed
    if mod and type(mod) == "table" and mod._before_unload then
        local ok, err = pcall(mod._before_unload)
        if not ok then
            log.warn(string.format("_before_unload failed for plugin %s: %s", name, tostring(err)))
        end
    end
    cleanup_plugin_capabilities(entry.key or name, entry)
    shutdown_plugin_worker(entry.key or name, "unload")

    -- Clear MCP tools/prompts registered by this plugin (source = "@" .. path).
    -- No begin_batch/end_batch needed: we only remove (never re-register), so
    -- exactly one notification fires — unlike reload_plugin which suppresses the
    -- intermediate "tools cleared" notification before re-registering.
    if mcp then
        mcp.reset("@" .. entry.path)
    end

    -- Remove plugin's lua/ dir from package.path and clear namespace modules
    -- so stale require() cache doesn't survive unload
    local root_dir = plugin_dir(entry.path)
    remove_from_package_path(root_dir)
    remove_from_package_path(root_dir .. "/lua")
    clear_recorded_modules(entry)
    clear_plugin_namespace(plugin_name)
    package.loaded[module_key] = nil

    -- Remove from registry
    registry[name] = nil

    log.info(string.format("Unloaded plugin: %s", name))
    return true
end

--- Disable a plugin. Unloads it if currently loaded, persists across restarts.
-- @param name string Plugin name
-- @return boolean success
-- @return string|nil error message
function M.disable_plugin(name)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})

    if not registry[name] then
        return false, "Plugin not found in registry: " .. name
    end

    -- Mark as disabled
    local S = M.get_disabled_set()
    S[name] = true
    save_disabled_set()

    -- Save path before unload removes the registry entry
    local saved_path = registry[name].path

    -- Unload if currently loaded
    local module_key = "plugin." .. name
    if package.loaded[module_key] then
        M.unload_plugin(name)
    end

    -- Re-add to registry (unload_plugin removes it, but we need it for enable)
    registry[name] = { path = saved_path, reload_count = 0 }
    update_registry_status(name, "disabled", nil)
    log.info(string.format("Disabled plugin: %s", name))
    return true
end

--- Enable a previously disabled plugin. Loads it immediately.
-- @param name string Plugin name
-- @return boolean success
-- @return string|nil error message
function M.enable_plugin(name)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})

    if not registry[name] then
        return false, "Plugin not found in registry: " .. name
    end

    -- Remove from disabled set
    local S = M.get_disabled_set()
    S[name] = nil
    save_disabled_set()

    -- Load the plugin
    local entry = registry[name]
    local ok, err = M.load_plugin(entry.path, entry.name or name, {
        source = entry.source,
        repo_root = entry.repo_root,
    })
    if ok then
        update_registry_status(name, "loaded", nil)
    else
        update_registry_status(name, "errored", err)
    end
    return ok, err
end

--- Get status of all plugins.
-- @return table Array of {name, path, status, error, loaded_at, reload_count}
function M.list_plugins()
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    local result = {}
    for name, entry in pairs(registry) do
        table.insert(result, {
            key = entry.key or name,
            name = entry.name or name,
            path = entry.path,
            source = entry.source,
            repo_root = entry.repo_root,
            status = entry.status or "unknown",
            error = entry.error,
            loaded_at = entry.loaded_at,
            error_at = entry.error_at,
            reload_count = entry.reload_count or 0,
        })
    end
    table.sort(result, function(a, b) return (a.key or a.name) < (b.key or b.name) end)
    return result
end

--- Get log entries for a plugin.
-- @param name string Plugin name
-- @param opts table|nil {level=string, limit=number}
-- @return table Array of log entries
function M.get_plugin_logs(name, opts)
    opts = opts or {}
    local ring = plugin_logs[name]
    if not ring then return {} end

    local result = {}
    for i = #ring, 1, -1 do
        local entry = ring[i]
        if not opts.level or entry.level == opts.level then
            table.insert(result, entry)
            if opts.limit and #result >= opts.limit then break end
        end
    end
    -- Reverse to chronological order
    local reversed = {}
    for i = #result, 1, -1 do
        table.insert(reversed, result[i])
    end
    return reversed
end

--- Clear log entries for a plugin.
-- @param name string Plugin name
function M.clear_plugin_logs(name)
    plugin_logs[name] = {}
end

-- ============================================================================
-- Trust Tiers / Sandboxing
-- ============================================================================

--- Create a restricted fs table that only allows access under base_dir.
-- Paths outside base_dir are rejected.
-- @param base_dir string The allowed base directory
-- @return table Restricted fs table
local function create_restricted_fs(base_dir)
    -- Normalize: ensure trailing slash for prefix checking
    local prefix = base_dir:gsub("/$", "") .. "/"

    local function check_path(path)
        -- Resolve ".." to prevent escape
        -- Simple check: path must start with the base_dir prefix
        if path:find(prefix, 1, true) ~= 1 and path ~= base_dir:gsub("/$", "") then
            return nil, string.format("Access denied: path outside %s", base_dir)
        end
        -- Block path traversal
        if path:find("%.%./") or path:find("%.%.$") then
            return nil, "Access denied: path traversal not allowed"
        end
        return true
    end

    return {
        exists = function(path)
            local ok, err = check_path(path)
            if not ok then
                log.warn("sandbox fs.exists: " .. err)
                return false
            end
            return fs.exists(path)
        end,
        read = function(path)
            local ok, err = check_path(path)
            if not ok then return nil, err end
            return fs.read(path)
        end,
        write = function(path, content)
            local ok, err = check_path(path)
            if not ok then return nil, err end
            return fs.write(path, content)
        end,
        listdir = function(path)
            local ok, err = check_path(path)
            if not ok then return nil, err end
            return fs.listdir(path)
        end,
        is_dir = function(path)
            local ok, err = check_path(path)
            if not ok then
                log.warn("sandbox fs.is_dir: " .. err)
                return false
            end
            return fs.is_dir(path)
        end,
        -- copy not exposed: agent code shouldn't copy arbitrary files
    }
end

--- Build a sandbox environment for agent/improvement code.
-- Provides safe access to hooks, logging, and read-only hub access.
-- Blocks: pty, webrtc, tui, worktree, unrestricted fs.
-- @param improvements_dir string The directory improvements can access
-- @return table The sandbox environment
local function build_sandbox(improvements_dir)
    local sandbox = {}

    -- Safe primitives (full access)
    sandbox.log = log
    sandbox.hooks = hooks
    sandbox.events = events

    -- json/timer may not exist yet; expose if available
    -- NOTE: http is intentionally excluded — agent code should not make
    -- arbitrary network requests (data exfiltration risk)
    if json then sandbox.json = json end
    if timer then sandbox.timer = timer end

    -- Read-only hub access
    if hub then
        sandbox.hub = { get_worktrees = hub.get_worktrees }
    end

    -- config: read-only (no set)
    if config then
        sandbox.config = {
            get = config.get,
            all = config.all,
        }
        if config.lua_path then sandbox.config.lua_path = config.lua_path end
        if config.data_dir then sandbox.config.data_dir = config.data_dir end
    end

    -- Restricted fs: only the improvements directory
    sandbox.fs = create_restricted_fs(improvements_dir)

    -- Standard Lua builtins
    sandbox.string = string
    sandbox.table = table
    sandbox.math = math
    sandbox.os = { time = os.time, date = os.date, clock = os.clock, difftime = os.difftime }
    sandbox.pairs = pairs
    sandbox.ipairs = ipairs
    sandbox.next = next
    sandbox.tostring = tostring
    sandbox.tonumber = tonumber
    sandbox.type = type
    sandbox.select = select
    sandbox.pcall = pcall
    sandbox.xpcall = xpcall
    sandbox.error = error
    sandbox.assert = assert
    sandbox.print = print
    sandbox.unpack = table.unpack
    sandbox.rawget = rawget
    sandbox.rawset = rawset
    sandbox.rawlen = rawlen
    sandbox.setmetatable = setmetatable
    sandbox.getmetatable = getmetatable

    -- No require: agent code cannot load arbitrary modules
    -- No io, no os.execute, no debug, no loadfile, no dofile

    return sandbox
end

--- Load a Lua file in a sandboxed environment.
-- Uses Lua 5.4's load() with custom _ENV for isolation.
-- @param path string The file path to load
-- @param improvements_dir string The directory the sandbox can access
-- @return boolean success
-- @return any error message on failure
function M.load_sandboxed(path, improvements_dir)
    local source, read_err = fs.read(path)
    if not source then
        return false, string.format("Cannot read %s: %s", path, tostring(read_err))
    end

    local sandbox = build_sandbox(improvements_dir)

    -- Lua 5.4: load(chunk, chunkname, mode, env)
    -- "t" mode = text only (no bytecode for safety)
    local chunk, err = load(source, "@" .. path, "t", sandbox)
    if not chunk then
        return false, string.format("Syntax error in %s: %s", path, tostring(err))
    end

    local ok, run_err = pcall(chunk)
    if not ok then
        return false, string.format("Runtime error in %s: %s", path, tostring(run_err))
    end

    return true
end

--- Load all improvement files from a directory with sandboxing.
-- Scans for .lua files and loads each in a restricted environment.
-- @param dir string The improvements directory path
-- @return number Number of improvements loaded
function M.load_improvements(dir)
    if not fs.exists(dir) then
        return 0
    end

    local entries, err = fs.listdir(dir)
    if not entries then
        log.warn(string.format("Failed to scan improvements directory %s: %s", dir, tostring(err)))
        return 0
    end

    local count = 0
    local names = {}
    for _, name in ipairs(entries) do
        if name:match("%.lua$") then
            table.insert(names, name)
        end
    end
    table.sort(names)

    for _, name in ipairs(names) do
        local path = dir .. "/" .. name
        local ok, load_err = M.load_sandboxed(path, dir)
        if ok then
            log.info(string.format("Loaded improvement: %s", name))
            count = count + 1
        else
            log.error(string.format("Failed to load improvement %s: %s", name, tostring(load_err)))
        end
    end

    return count
end

return M
