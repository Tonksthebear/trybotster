-- Module loader with hot-reload support and trust tiers
--
-- Trust levels:
--   core  - Protected from reload, full access
--   user  - Full access to all primitives (plugins, user/init.lua)
--   agent - Restricted: no process spawn, no keyring, fs limited to improvements/
local M = {}

-- Track which modules should never be reloaded
local protected_modules = {
    ["core.state"] = true,
    ["core.hooks"] = true,
    ["core.loader"] = true,
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

--- Discover plugins in a directory.
-- Scans for subdirectories containing init.lua.
-- Returns a sorted list of plugin names.
-- @param dir string The plugins directory path
-- @return table Array of plugin names (sorted)
function M.discover_plugins(dir)
    local plugins = {}

    if not fs.exists(dir) then
        return plugins
    end

    local entries, err = fs.listdir(dir)
    if not entries then
        log.warn(string.format("Failed to scan plugins directory %s: %s", dir, tostring(err)))
        return plugins
    end

    for _, name in ipairs(entries) do
        local plugin_dir = dir .. "/" .. name
        local init_path = plugin_dir .. "/init.lua"
        if fs.is_dir(plugin_dir) and fs.exists(init_path) then
            table.insert(plugins, name)
        end
    end

    table.sort(plugins)
    return plugins
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
