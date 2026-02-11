-- Config resolver for .botster/ directory structure.
--
-- Merges shared/ config with a selected profile to produce a resolved
-- session list and file paths. Pure-data module, no side effects.
--
-- Directory structure:
--   .botster/
--     shared/                        # merged into EVERY profile
--       workspace_include            # glob patterns for file copying
--       workspace_teardown           # cleanup script
--       sessions/
--         agent/                     # REQUIRED, always index 0
--           initialization           # startup script
--     profiles/                      # at least one required
--       standard/
--         sessions/
--           server/
--             initialization
--             port_forward           # sentinel = gets $PORT
--
-- Resolution rules:
-- - Resolved config = shared/ merged with selected profile
-- - Profile files win on collision (session dirs, workspace_include, etc.)
-- - "agent" session required (in shared or profile), always sorted first
-- - port_forward is opt-in via sentinel file existence
--
-- This module is hot-reloadable.

local M = {}

-- =============================================================================
-- Internal Helpers
-- =============================================================================

--- List subdirectory names under a path (skips files).
-- @param path string Directory to scan
-- @return string[] Array of directory names, or empty table
local function list_subdirs(path)
    if not fs.exists(path) or not fs.is_dir(path) then
        return {}
    end
    local entries, err = fs.listdir(path)
    if not entries then
        if err then
            log.warn(string.format("ConfigResolver: failed to list %s: %s", path, err))
        end
        return {}
    end
    local dirs = {}
    for _, name in ipairs(entries) do
        if fs.is_dir(path .. "/" .. name) then
            dirs[#dirs + 1] = name
        end
    end
    return dirs
end

--- Read sessions from a sessions/ directory.
-- @param sessions_dir string Path to sessions/ directory
-- @return table Map of session_name -> { initialization, port_forward }
local function read_sessions(sessions_dir)
    local result = {}
    local session_names = list_subdirs(sessions_dir)
    for _, name in ipairs(session_names) do
        local session_path = sessions_dir .. "/" .. name
        local init_path = session_path .. "/initialization"
        local has_init = fs.exists(init_path)
        local has_port_forward = fs.exists(session_path .. "/port_forward")

        if has_init then
            result[name] = {
                initialization = init_path,
                port_forward = has_port_forward,
            }
        else
            log.warn(string.format(
                "ConfigResolver: session '%s' at %s has no initialization file, skipping",
                name, session_path))
        end
    end
    return result
end

-- =============================================================================
-- Public API
-- =============================================================================

--- Check if an agent session exists in shared layers (without needing a profile).
-- Checks both device shared and repo shared for an agent/initialization file.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return boolean
function M.has_agent_without_profile(device_root, repo_root)
    if device_root and fs.exists(device_root .. "/shared/sessions/agent/initialization") then
        return true
    end
    if repo_root and fs.exists(repo_root .. "/.botster/shared/sessions/agent/initialization") then
        return true
    end
    return false
end

-- =============================================================================
-- Unified Multi-Scope Resolution
-- =============================================================================
-- Resolves config across 4 layers (most specific wins):
--   1. device shared   (~/.botster/shared/)
--   2. device profile  (~/.botster/profiles/{profile}/)
--   3. repo shared     ({repo}/.botster/shared/)
--   4. repo profile    ({repo}/.botster/profiles/{profile}/)

--- Source labels for provenance tracking.
local SOURCES = {
    device_shared = "device_shared",
    device_profile = "device_profile",
    repo_shared = "repo_shared",
    repo_profile = "repo_profile",
}

--- Read plugins from a base path's plugins/ directory.
-- Scans {base}/plugins/*/init.lua and returns a map of name -> { init_path }.
-- @param base_path string Directory containing plugins/
-- @return table Map of plugin_name -> { init_path }
function M.read_plugins(base_path)
    local plugins_dir = base_path .. "/plugins"
    local result = {}
    local plugin_names = list_subdirs(plugins_dir)
    for _, name in ipairs(plugin_names) do
        local init_path = plugins_dir .. "/" .. name .. "/init.lua"
        if fs.exists(init_path) then
            result[name] = { init_path = init_path }
        end
    end
    return result
end

--- Read a single layer's config (sessions, plugins, workspace files).
-- @param base_path string Path to a shared/ or profiles/{name}/ dir
-- @param source string Source label from SOURCES
-- @return table { sessions, plugins, workspace_include, workspace_teardown }
local function read_layer(base_path, source)
    local layer = {
        sessions = {},
        plugins = {},
        workspace_include = nil,
        workspace_teardown = nil,
    }

    if not fs.exists(base_path) or not fs.is_dir(base_path) then
        return layer
    end

    -- Sessions
    local raw_sessions = read_sessions(base_path .. "/sessions")
    for name, session in pairs(raw_sessions) do
        layer.sessions[name] = {
            name = name,
            initialization = session.initialization,
            port_forward = session.port_forward,
            source = source,
        }
    end

    -- Plugins
    local raw_plugins = M.read_plugins(base_path)
    for name, plugin in pairs(raw_plugins) do
        layer.plugins[name] = {
            name = name,
            init_path = plugin.init_path,
            source = source,
        }
    end

    -- Workspace files
    local wi_path = base_path .. "/workspace_include"
    if fs.exists(wi_path) then
        layer.workspace_include = { path = wi_path, source = source }
    end

    local wt_path = base_path .. "/workspace_teardown"
    if fs.exists(wt_path) then
        layer.workspace_teardown = { path = wt_path, source = source }
    end

    return layer
end

--- Merge a higher-priority layer into an accumulator.
-- Higher-priority values overwrite lower-priority ones.
-- @param acc table Accumulator (mutated in place)
-- @param layer table Layer to merge in
local function merge_layer(acc, layer)
    -- Sessions: merge by name, higher priority wins
    for name, session in pairs(layer.sessions) do
        acc.sessions[name] = session
    end

    -- Plugins: merge by name, higher priority wins
    for name, plugin in pairs(layer.plugins) do
        acc.plugins[name] = plugin
    end

    -- Workspace files: higher priority wins (single value)
    if layer.workspace_include then
        acc.workspace_include = layer.workspace_include
    end
    if layer.workspace_teardown then
        acc.workspace_teardown = layer.workspace_teardown
    end
end

--- Resolve config across all 4 layers (device shared, device profile, repo shared, repo profile).
-- @param opts table { device_root, repo_root, profile, require_agent }
--   device_root: path to ~/.botster (nil to skip device layers)
--   repo_root: path to repo root (nil to skip repo layers)
--   profile: profile name (nil for shared-only)
--   require_agent: require agent session in merged result (default true)
-- @return table { sessions[], plugins[], workspace_include, workspace_teardown } or nil, error
function M.resolve_all(opts)
    local device_root = opts.device_root
    local repo_root = opts.repo_root
    local profile = opts.profile

    local acc = {
        sessions = {},
        plugins = {},
        workspace_include = nil,
        workspace_teardown = nil,
    }

    -- Layer 1: device shared
    if device_root then
        merge_layer(acc, read_layer(device_root .. "/shared", SOURCES.device_shared))
    end

    -- Layer 2: device profile
    if device_root and profile then
        local dp = device_root .. "/profiles/" .. profile
        if fs.exists(dp) and fs.is_dir(dp) then
            merge_layer(acc, read_layer(dp, SOURCES.device_profile))
        end
    end

    -- Layer 3: repo shared
    if repo_root then
        merge_layer(acc, read_layer(repo_root .. "/.botster/shared", SOURCES.repo_shared))
    end

    -- Layer 4: repo profile
    if repo_root and profile then
        local rp = repo_root .. "/.botster/profiles/" .. profile
        if fs.exists(rp) and fs.is_dir(rp) then
            merge_layer(acc, read_layer(rp, SOURCES.repo_profile))
        end
    end

    -- Validate: agent session must exist in merged result (unless opted out)
    local require_agent = opts.require_agent ~= false  -- default true
    if require_agent and not acc.sessions.agent then
        return nil, "No 'agent' session found in any config layer. " ..
            "An agent session with an initialization file is required."
    end

    -- Build sorted sessions array (agent first if present, then alphabetical)
    local sessions_array = {}
    local other_names = {}
    for name, _ in pairs(acc.sessions) do
        if name ~= "agent" then
            other_names[#other_names + 1] = name
        end
    end
    table.sort(other_names)

    if acc.sessions.agent then
        sessions_array[1] = acc.sessions.agent
    end
    for _, name in ipairs(other_names) do
        sessions_array[#sessions_array + 1] = acc.sessions[name]
    end

    -- Build sorted plugins array
    local plugins_array = {}
    local plugin_names = {}
    for name, _ in pairs(acc.plugins) do
        plugin_names[#plugin_names + 1] = name
    end
    table.sort(plugin_names)
    for _, name in ipairs(plugin_names) do
        plugins_array[#plugins_array + 1] = acc.plugins[name]
    end

    return {
        sessions = sessions_array,
        plugins = plugins_array,
        workspace_include = acc.workspace_include,
        workspace_teardown = acc.workspace_teardown,
    }
end

--- List all profiles across device and repo scopes.
-- Returns the union of profile directory names from both locations.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return string[] Sorted, deduplicated profile names
function M.list_profiles_all(device_root, repo_root)
    local seen = {}
    local result = {}

    local function add_profiles(root)
        if not root then return end
        local names = list_subdirs(root)
        for _, name in ipairs(names) do
            if not seen[name] then
                seen[name] = true
                result[#result + 1] = name
            end
        end
    end

    if device_root then
        add_profiles(device_root .. "/profiles")
    end
    if repo_root then
        add_profiles(repo_root .. "/.botster/profiles")
    end

    table.sort(result)
    return result
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================

function M._before_reload()
    log.info("config_resolver.lua reloading")
end

function M._after_reload()
    log.info("config_resolver.lua reloaded")
end

return M
