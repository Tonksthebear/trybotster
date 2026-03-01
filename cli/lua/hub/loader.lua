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

-- Add a plugin's lua/ dir to package.path (idempotent).
local function add_to_package_path(lua_dir)
    local entry1 = lua_dir .. "/?.lua"
    local entry2 = lua_dir .. "/?/init.lua"
    if not package.path:find(entry1, 1, true) then
        package.path = entry1 .. ";" .. entry2 .. ";" .. package.path
    end
end

-- Remove a plugin's lua/ dir from package.path.
local function remove_from_package_path(lua_dir)
    local entry1 = lua_dir .. "/?.lua"
    local entry2 = lua_dir .. "/?/init.lua"
    local parts = {}
    for part in (package.path .. ";"):gmatch("([^;]*);") do
        if part ~= entry1 and part ~= entry2 and part ~= "" then
            table.insert(parts, part)
        end
    end
    package.path = table.concat(parts, ";")
end

-- Clear all package.loaded entries belonging to a plugin's namespace (e.g. "telegram.*").
local function clear_plugin_namespace(name)
    local prefix = name .. "."
    for key in pairs(package.loaded) do
        if key:sub(1, #prefix) == prefix then
            package.loaded[key] = nil
        end
    end
end

--- Load a plugin by absolute path (not via require/package.path).
-- Loads the file with full _ENV (same trust as user plugins), registers
-- it in package.loaded so it can be reloaded by name.
-- If the plugin directory contains a lua/ subdir, it is added to package.path
-- so the plugin can require() its own modules (e.g. require("telegram.api")).
-- @param path string Absolute path to the plugin's init.lua
-- @param name string Plugin name (used for registration and logging)
-- @return boolean success
function M.load_plugin(path, name)
    if not fs.exists(path) then
        local msg = string.format("load_plugin: %s not found at %s", name, path)
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
        log.error(msg)
        return false, msg
    end

    -- Batch MCP notifications so N mcp.tool()/mcp.prompt() calls emit at most
    -- one notification each instead of N. end_batch() always runs (via pcall)
    -- so batch mode is never left stuck on load error.
    if mcp then mcp.begin_batch() end

    -- Set source context so mcp.tool() can track which plugin registered each tool
    _G._loading_plugin_source = "@" .. path
    local ok, result = pcall(chunk)
    _G._loading_plugin_source = nil

    if mcp then mcp.end_batch() end

    if not ok then
        local msg = string.format("load_plugin: runtime error in %s: %s", path, tostring(result))
        log.error(msg)
        return false, msg
    end

    -- Register in package.loaded so reload works
    local module_key = "plugin." .. name
    package.loaded[module_key] = result or true

    -- Add plugin's lua/ subdir to package.path if it exists, so the plugin
    -- can require() its own modules namespaced by plugin name.
    local plugin_dir = path:match("^(.*)/[^/]+$") or "."
    local lua_dir = plugin_dir .. "/lua"
    if fs.exists(lua_dir) and fs.is_dir(lua_dir) then
        add_to_package_path(lua_dir)
        log.info(string.format("Plugin %s: registered lua/ at %s", name, lua_dir))
    end

    log.info(string.format("Loaded plugin: %s from %s", name, path))
    return true
end

--- Reload a plugin by name using the runtime registry.
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
        return false, "Plugin not found in registry: " .. name
    end

    local module_key = "plugin." .. name
    local old = package.loaded[module_key]

    -- Lifecycle: cleanup before reload
    if old and type(old) == "table" and old._before_reload then
        local ok, err = pcall(old._before_reload)
        if not ok then
            log.warn(string.format("_before_reload failed for plugin %s: %s", name, tostring(err)))
        end
    end

    -- Batch MCP notifications: suppress mcp_tools_changed/mcp_prompts_changed during
    -- reset + re-registration, then emit exactly once per changed registry at the end.
    -- end_batch() runs even on load failure (registrations were cleared by reset,
    -- clients need one notification to reflect that).
    if mcp then mcp.begin_batch() end

    -- Clear MCP tools registered by this plugin (source = "@" .. path)
    if mcp then
        mcp.reset("@" .. entry.path)
    end

    -- Remove plugin's lua/ path and namespace modules so stale require() cache
    -- doesn't survive reload. load_plugin() re-adds the path after loading.
    -- Track whether the lua/ path was present so we can restore it on failure.
    local plugin_dir = entry.path:match("^(.*)/[^/]+$") or "."
    local lua_dir = plugin_dir .. "/lua"
    local had_lua_path = package.path:find(lua_dir .. "/?.lua", 1, true) ~= nil
    remove_from_package_path(lua_dir)
    clear_plugin_namespace(name)

    -- Clear old module
    package.loaded[module_key] = nil

    -- Re-load from disk (errors caught internally — load_plugin never throws)
    local ok = M.load_plugin(entry.path, name)

    -- Single notification for the entire reload cycle
    if mcp then mcp.end_batch() end

    if not ok then
        -- Restore old module and lua/ package.path on failure, symmetric with
        -- package.loaded restore above.
        package.loaded[module_key] = old
        if had_lua_path then
            add_to_package_path(lua_dir)
        end
        return false, "Failed to reload plugin: " .. name
    end

    return true
end

--- Unload a plugin by name, cleaning up package.path and loaded modules.
-- Runs _before_reload lifecycle hook, clears MCP registrations, removes the
-- plugin's lua/ dir from package.path, and clears its namespace from package.loaded.
-- @param name string Plugin name (e.g., "telegram")
function M.unload_plugin(name)
    local state = require("hub.state")
    local registry = state.get("plugin_registry", {})
    local entry = registry[name]

    local module_key = "plugin." .. name
    local old = package.loaded[module_key]

    -- Lifecycle hook
    if old and type(old) == "table" and old._before_reload then
        local ok, err = pcall(old._before_reload)
        if not ok then
            log.warn(string.format("_before_reload failed for plugin %s: %s", name, tostring(err)))
        end
    end

    -- Clear MCP registrations
    if mcp and entry then
        mcp.reset("@" .. entry.path)
    end

    -- Remove lua/ path and namespace modules
    if entry then
        local plugin_dir = entry.path:match("^(.*)/[^/]+$") or "."
        remove_from_package_path(plugin_dir .. "/lua")
    end
    clear_plugin_namespace(name)
    package.loaded[module_key] = nil

    log.info(string.format("Unloaded plugin: %s", name))
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
