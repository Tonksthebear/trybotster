-- Config resolver for .botster/ directory structure.
--
-- 2-layer merge: device (~/.botster/) + repo (.botster/), repo wins on collision.
-- Pure-data module, no side effects.
--
-- Directory structure:
--   .botster/
--     agents/
--       claude/
--         initialization           # startup script
--       codex/
--         initialization
--     accessories/
--       rails-server/
--         initialization
--         port_forward             # sentinel = gets $PORT
--     workspaces/
--       dev.json                   # { "agents": ["claude"], "accessories": ["rails-server"] }
--     plugins/
--       github/
--         init.lua
--     workspace_include            # glob patterns for file copying
--     workspace_teardown           # cleanup script
--
-- Resolution rules:
-- - 2-layer merge: device (~/.botster/) then repo (.botster/), repo wins
-- - At least one agent required (unless opted out)
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

--- List files matching a pattern under a path (skips directories).
-- @param path string Directory to scan
-- @param extension string|nil File extension to filter (e.g., ".json")
-- @return string[] Array of filenames
local function list_files(path, extension)
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
    local files = {}
    for _, name in ipairs(entries) do
        if not fs.is_dir(path .. "/" .. name) then
            if not extension or name:sub(-#extension) == extension then
                files[#files + 1] = name
            end
        end
    end
    return files
end

-- =============================================================================
-- Scan Functions (operate on a single .botster/ root)
-- =============================================================================

--- Scan agents/ directory: agents/*/initialization
-- @param botster_root string Path to a .botster/ directory
-- @param source string Source label ("device" or "repo")
-- @return table Map of agent_name -> { name, initialization, source }
local function read_agents(botster_root, source)
    local agents_dir = botster_root .. "/agents"
    local result = {}
    local names = list_subdirs(agents_dir)
    for _, name in ipairs(names) do
        local init_path = agents_dir .. "/" .. name .. "/initialization"
        if fs.exists(init_path) then
            result[name] = {
                name = name,
                initialization = init_path,
                source = source,
            }
        else
            log.warn(string.format(
                "ConfigResolver: agent '%s' at %s has no initialization file, skipping",
                name, agents_dir .. "/" .. name))
        end
    end
    return result
end

--- Scan accessories/ directory: accessories/*/initialization + port_forward sentinel
-- @param botster_root string Path to a .botster/ directory
-- @param source string Source label ("device" or "repo")
-- @return table Map of accessory_name -> { name, initialization, port_forward, source }
local function read_accessories(botster_root, source)
    local accessories_dir = botster_root .. "/accessories"
    local result = {}
    local names = list_subdirs(accessories_dir)
    for _, name in ipairs(names) do
        local acc_path = accessories_dir .. "/" .. name
        local init_path = acc_path .. "/initialization"
        if fs.exists(init_path) then
            result[name] = {
                name = name,
                initialization = init_path,
                port_forward = fs.exists(acc_path .. "/port_forward"),
                source = source,
            }
        else
            log.warn(string.format(
                "ConfigResolver: accessory '%s' at %s has no initialization file, skipping",
                name, acc_path))
        end
    end
    return result
end

--- Scan workspaces/ directory: workspaces/*.json
-- @param botster_root string Path to a .botster/ directory
-- @param source string Source label ("device" or "repo")
-- @return table Map of workspace_name -> { name, agents[], accessories[], source }
local function read_workspaces(botster_root, source)
    local workspaces_dir = botster_root .. "/workspaces"
    local result = {}
    local files = list_files(workspaces_dir, ".json")
    for _, filename in ipairs(files) do
        local name = filename:sub(1, -6)  -- strip .json
        local file_path = workspaces_dir .. "/" .. filename
        local content = fs.read(file_path)
        if content then
            local ok, parsed = pcall(json.decode, content)
            if ok and parsed then
                result[name] = {
                    name = name,
                    agents = parsed.agents or {},
                    accessories = parsed.accessories or {},
                    source = source,
                }
            else
                log.warn(string.format(
                    "ConfigResolver: workspace '%s' has invalid JSON, skipping", name))
            end
        end
    end
    return result
end

--- Scan plugins/ directory: plugins/*/init.lua
-- @param botster_root string Path to a .botster/ directory
-- @param source string Source label ("device" or "repo")
-- @return table Map of plugin_name -> { name, init_path, source }
local function read_plugins(botster_root, source)
    local plugins_dir = botster_root .. "/plugins"
    local result = {}
    local names = list_subdirs(plugins_dir)
    for _, name in ipairs(names) do
        local init_path = plugins_dir .. "/" .. name .. "/init.lua"
        if fs.exists(init_path) then
            result[name] = {
                name = name,
                init_path = init_path,
                source = source,
            }
        end
    end
    return result
end

-- =============================================================================
-- Public API
-- =============================================================================

--- Read plugins from a base path's plugins/ directory.
-- Kept for backward compatibility with templates.lua scan_layer calls.
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

--- Resolve config across 2 layers (device, repo). Repo wins on collision.
-- @param opts table { device_root, repo_root, require_agent }
--   device_root: path to ~/.botster (nil to skip device layer)
--   repo_root: path to repo root (nil to skip repo layer)
--   require_agent: require at least one agent in merged result (default true)
-- @return table { agents{}, accessories{}, workspaces{}, plugins[], workspace_include, workspace_teardown } or nil, error
function M.resolve_all(opts)
    local device_root = opts.device_root
    local repo_root = opts.repo_root

    local acc = {
        agents = {},
        accessories = {},
        workspaces = {},
        plugins = {},
        workspace_include = nil,
        workspace_teardown = nil,
    }

    -- Layer 1: device (~/.botster/)
    if device_root then
        local dr = device_root
        for name, agent in pairs(read_agents(dr, "device")) do
            acc.agents[name] = agent
        end
        for name, accessory in pairs(read_accessories(dr, "device")) do
            acc.accessories[name] = accessory
        end
        for name, workspace in pairs(read_workspaces(dr, "device")) do
            acc.workspaces[name] = workspace
        end
        for name, plugin in pairs(read_plugins(dr, "device")) do
            acc.plugins[name] = plugin
        end
        local wi_path = dr .. "/workspace_include"
        if fs.exists(wi_path) then
            acc.workspace_include = { path = wi_path, source = "device" }
        end
        local wt_path = dr .. "/workspace_teardown"
        if fs.exists(wt_path) then
            acc.workspace_teardown = { path = wt_path, source = "device" }
        end
    end

    -- Layer 2: repo ({repo}/.botster/) — wins on collision
    if repo_root then
        local rr = repo_root .. "/.botster"
        for name, agent in pairs(read_agents(rr, "repo")) do
            acc.agents[name] = agent
        end
        for name, accessory in pairs(read_accessories(rr, "repo")) do
            acc.accessories[name] = accessory
        end
        for name, workspace in pairs(read_workspaces(rr, "repo")) do
            acc.workspaces[name] = workspace
        end
        for name, plugin in pairs(read_plugins(rr, "repo")) do
            acc.plugins[name] = plugin
        end
        local wi_path = rr .. "/workspace_include"
        if fs.exists(wi_path) then
            acc.workspace_include = { path = wi_path, source = "repo" }
        end
        local wt_path = rr .. "/workspace_teardown"
        if fs.exists(wt_path) then
            acc.workspace_teardown = { path = wt_path, source = "repo" }
        end
    end

    -- Validate: at least one agent must exist (unless opted out)
    local require_agent = opts.require_agent ~= false  -- default true
    if require_agent then
        local has_agent = false
        for _ in pairs(acc.agents) do
            has_agent = true
            break
        end
        if not has_agent then
            return nil, "No agents found in any config layer. " ..
                "At least one agent with an initialization file is required."
        end
    end

    -- Build sorted plugins array (for ordered iteration)
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
        agents = acc.agents,
        accessories = acc.accessories,
        workspaces = acc.workspaces,
        plugins = plugins_array,
        workspace_include = acc.workspace_include,
        workspace_teardown = acc.workspace_teardown,
    }
end

--- List all agent names across device and repo scopes.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return string[] Sorted, deduplicated agent names
function M.list_agents(device_root, repo_root)
    local seen = {}
    local result = {}
    if device_root then
        for _, name in ipairs(list_subdirs(device_root .. "/agents")) do
            if fs.exists(device_root .. "/agents/" .. name .. "/initialization") then
                if not seen[name] then
                    seen[name] = true
                    result[#result + 1] = name
                end
            end
        end
    end
    if repo_root then
        local rr = repo_root .. "/.botster/agents"
        for _, name in ipairs(list_subdirs(rr)) do
            if fs.exists(rr .. "/" .. name .. "/initialization") then
                if not seen[name] then
                    seen[name] = true
                    result[#result + 1] = name
                end
            end
        end
    end
    table.sort(result)
    return result
end

--- List all accessory names across device and repo scopes.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return string[] Sorted, deduplicated accessory names
function M.list_accessories(device_root, repo_root)
    local seen = {}
    local result = {}
    if device_root then
        for _, name in ipairs(list_subdirs(device_root .. "/accessories")) do
            if fs.exists(device_root .. "/accessories/" .. name .. "/initialization") then
                if not seen[name] then
                    seen[name] = true
                    result[#result + 1] = name
                end
            end
        end
    end
    if repo_root then
        local rr = repo_root .. "/.botster/accessories"
        for _, name in ipairs(list_subdirs(rr)) do
            if fs.exists(rr .. "/" .. name .. "/initialization") then
                if not seen[name] then
                    seen[name] = true
                    result[#result + 1] = name
                end
            end
        end
    end
    table.sort(result)
    return result
end

--- List all workspace names across device and repo scopes.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return string[] Sorted, deduplicated workspace names
function M.list_workspaces(device_root, repo_root)
    local seen = {}
    local result = {}
    local function scan(dir)
        if not dir then return end
        for _, filename in ipairs(list_files(dir, ".json")) do
            local name = filename:sub(1, -6)
            if not seen[name] then
                seen[name] = true
                result[#result + 1] = name
            end
        end
    end
    if device_root then
        scan(device_root .. "/workspaces")
    end
    if repo_root then
        scan(repo_root .. "/.botster/workspaces")
    end
    table.sort(result)
    return result
end

-- =============================================================================
-- Backward Compatibility: Migration Detection & Helpers
-- =============================================================================

--- Check if the old profiles/ or shared/sessions/ structure exists.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return boolean true if legacy structure is detected
function M.needs_migration(device_root, repo_root)
    if device_root then
        if fs.exists(device_root .. "/profiles") and fs.is_dir(device_root .. "/profiles") then
            return true
        end
        if fs.exists(device_root .. "/shared/sessions") and fs.is_dir(device_root .. "/shared/sessions") then
            return true
        end
    end
    if repo_root then
        local rr = repo_root .. "/.botster"
        if fs.exists(rr .. "/profiles") and fs.is_dir(rr .. "/profiles") then
            return true
        end
        if fs.exists(rr .. "/shared/sessions") and fs.is_dir(rr .. "/shared/sessions") then
            return true
        end
    end
    return false
end

--- Migrate old profiles/shared structure to new agents/accessories layout.
-- Moves files in-place. Safe to call multiple times (idempotent).
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return boolean ok
-- @return string|nil error
function M.migrate(device_root, repo_root)
    local function migrate_root(root)
        if not root then return end

        -- Migrate shared/sessions/* → agents/* (session named "agent" becomes the agent)
        local shared_sessions = root .. "/shared/sessions"
        if fs.exists(shared_sessions) and fs.is_dir(shared_sessions) then
            local names = list_subdirs(shared_sessions)
            for _, name in ipairs(names) do
                local src = shared_sessions .. "/" .. name
                local dest_dir
                if name == "agent" then
                    -- shared/sessions/agent → agents/default
                    dest_dir = root .. "/agents/default"
                else
                    -- shared/sessions/server → accessories/server
                    dest_dir = root .. "/accessories/" .. name
                end
                if not fs.exists(dest_dir) then
                    fs.mkdir(dest_dir)
                    -- Copy files from src to dest
                    local entries = fs.listdir(src) or {}
                    for _, file in ipairs(entries) do
                        if not fs.is_dir(src .. "/" .. file) then
                            local content = fs.read(src .. "/" .. file)
                            if content then
                                fs.write(dest_dir .. "/" .. file, content)
                            end
                        end
                    end
                    log.info(string.format("ConfigResolver: migrated %s → %s", src, dest_dir))
                end
            end
        end

        -- Migrate profiles/*/sessions/agent → agents/*/
        local profiles_dir = root .. "/profiles"
        if fs.exists(profiles_dir) and fs.is_dir(profiles_dir) then
            local profile_names = list_subdirs(profiles_dir)
            for _, profile_name in ipairs(profile_names) do
                local profile_sessions = profiles_dir .. "/" .. profile_name .. "/sessions"
                if fs.exists(profile_sessions) and fs.is_dir(profile_sessions) then
                    local session_names = list_subdirs(profile_sessions)
                    for _, sess_name in ipairs(session_names) do
                        local src = profile_sessions .. "/" .. sess_name
                        local dest_dir
                        if sess_name == "agent" then
                            -- profiles/claude/sessions/agent → agents/claude
                            dest_dir = root .. "/agents/" .. profile_name
                        else
                            -- profiles/claude/sessions/server → accessories/server
                            dest_dir = root .. "/accessories/" .. sess_name
                        end
                        if not fs.exists(dest_dir) then
                            fs.mkdir(dest_dir)
                            local entries = fs.listdir(src) or {}
                            for _, file in ipairs(entries) do
                                if not fs.is_dir(src .. "/" .. file) then
                                    local content = fs.read(src .. "/" .. file)
                                    if content then
                                        fs.write(dest_dir .. "/" .. file, content)
                                    end
                                end
                            end
                            log.info(string.format("ConfigResolver: migrated %s → %s", src, dest_dir))
                        end
                    end
                end

                -- Migrate profile-level plugins → top-level plugins
                local profile_plugins = profiles_dir .. "/" .. profile_name .. "/plugins"
                if fs.exists(profile_plugins) and fs.is_dir(profile_plugins) then
                    local plugin_names = list_subdirs(profile_plugins)
                    for _, plugin_name in ipairs(plugin_names) do
                        local src = profile_plugins .. "/" .. plugin_name
                        local dest = root .. "/plugins/" .. plugin_name
                        if not fs.exists(dest) then
                            fs.mkdir(dest)
                            local entries = fs.listdir(src) or {}
                            for _, file in ipairs(entries) do
                                if not fs.is_dir(src .. "/" .. file) then
                                    local content = fs.read(src .. "/" .. file)
                                    if content then
                                        fs.write(dest .. "/" .. file, content)
                                    end
                                end
                            end
                            log.info(string.format("ConfigResolver: migrated plugin %s → %s", src, dest))
                        end
                    end
                end
            end
        end

        -- Migrate shared/plugins → plugins (if not already there)
        local shared_plugins = root .. "/shared/plugins"
        if fs.exists(shared_plugins) and fs.is_dir(shared_plugins) then
            local plugin_names = list_subdirs(shared_plugins)
            for _, plugin_name in ipairs(plugin_names) do
                local src = shared_plugins .. "/" .. plugin_name
                local dest = root .. "/plugins/" .. plugin_name
                if not fs.exists(dest) then
                    fs.mkdir(dest)
                    local entries = fs.listdir(src) or {}
                    for _, file in ipairs(entries) do
                        if not fs.is_dir(src .. "/" .. file) then
                            local content = fs.read(src .. "/" .. file)
                            if content then
                                fs.write(dest .. "/" .. file, content)
                            end
                        end
                    end
                    log.info(string.format("ConfigResolver: migrated plugin %s → %s", src, dest))
                end
            end
        end

        -- Migrate shared/workspace_include → workspace_include
        local shared_wi = root .. "/shared/workspace_include"
        local top_wi = root .. "/workspace_include"
        if fs.exists(shared_wi) and not fs.exists(top_wi) then
            local content = fs.read(shared_wi)
            if content then
                fs.write(top_wi, content)
                log.info(string.format("ConfigResolver: migrated %s → %s", shared_wi, top_wi))
            end
        end

        -- Migrate shared/workspace_teardown → workspace_teardown
        local shared_wt = root .. "/shared/workspace_teardown"
        local top_wt = root .. "/workspace_teardown"
        if fs.exists(shared_wt) and not fs.exists(top_wt) then
            local content = fs.read(shared_wt)
            if content then
                fs.write(top_wt, content)
                log.info(string.format("ConfigResolver: migrated %s → %s", shared_wt, top_wt))
            end
        end
    end

    migrate_root(device_root)
    if repo_root then
        migrate_root(repo_root .. "/.botster")
    end

    return true
end

-- =============================================================================
-- Backward Compatibility Shims (deprecated, will be removed)
-- =============================================================================

--- DEPRECATED: Check if an agent exists without needing a profile.
-- Now checks if any agent exists in the new structure.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return boolean
function M.has_agent_without_profile(device_root, repo_root)
    return #M.list_agents(device_root, repo_root) > 0
end

--- DEPRECATED: List all profiles. Now returns agent names.
-- @param device_root string|nil Path to ~/.botster
-- @param repo_root string|nil Path to repo root
-- @return string[] Agent names (as profile stand-ins)
function M.list_profiles_all(device_root, repo_root)
    return M.list_agents(device_root, repo_root)
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
