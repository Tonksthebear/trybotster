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

--- Resolve config for a given profile by merging shared/ + profile/.
-- When profile_name is nil, uses shared config only (no profile overlay).
-- @param repo_root string Repository root path
-- @param profile_name string|nil Profile name, or nil for shared-only
-- @return table { workspace_include, workspace_teardown, sessions[] } or nil, error
function M.resolve(repo_root, profile_name)
    assert(repo_root, "ConfigResolver.resolve requires repo_root")

    local botster_dir = repo_root .. "/.botster"

    -- Validate .botster/ directory exists
    if not fs.exists(botster_dir) or not fs.is_dir(botster_dir) then
        return nil, string.format(
            ".botster/ configuration directory not found at %s", botster_dir)
    end

    local shared_dir = botster_dir .. "/shared"
    local profile_dir = nil

    -- Validate profile exists (when specified)
    if profile_name then
        profile_dir = botster_dir .. "/profiles/" .. profile_name
        if not fs.exists(profile_dir) or not fs.is_dir(profile_dir) then
            return nil, string.format("Profile '%s' not found at %s", profile_name, profile_dir)
        end
    end

    -- 1. Read shared sessions
    local shared_sessions = read_sessions(shared_dir .. "/sessions")

    -- 2. Read profile sessions (if profile specified)
    local profile_sessions = profile_dir and read_sessions(profile_dir .. "/sessions") or {}

    -- 3. Merge: profile wins on collision
    local merged = {}
    for name, session in pairs(shared_sessions) do
        merged[name] = session
    end
    for name, session in pairs(profile_sessions) do
        merged[name] = session  -- profile overrides shared
    end

    -- 4. Validate: agent session must exist
    if not merged.agent then
        if profile_name then
            return nil, string.format(
                "No 'agent' session found in shared/ or profile '%s'. " ..
                "An agent session with an initialization file is required.",
                profile_name)
        else
            return nil, "No 'agent' session found in shared/. " ..
                "An agent session with an initialization file is required."
        end
    end

    -- 5. Sort: agent first, then alphabetical
    local session_names = {}
    for name, _ in pairs(merged) do
        if name ~= "agent" then
            session_names[#session_names + 1] = name
        end
    end
    table.sort(session_names)

    -- Build ordered sessions array
    local sessions = {}
    -- Agent always first (index 0 in Rust, index 1 in Lua)
    sessions[1] = {
        name = "agent",
        initialization = merged.agent.initialization,
        port_forward = merged.agent.port_forward,
    }
    for _, name in ipairs(session_names) do
        sessions[#sessions + 1] = {
            name = name,
            initialization = merged[name].initialization,
            port_forward = merged[name].port_forward,
        }
    end

    -- 6. Resolve workspace files (profile > shared)
    local workspace_include = nil
    local workspace_teardown = nil

    if profile_dir and fs.exists(profile_dir .. "/workspace_include") then
        workspace_include = profile_dir .. "/workspace_include"
    elseif fs.exists(shared_dir .. "/workspace_include") then
        workspace_include = shared_dir .. "/workspace_include"
    end

    if profile_dir and fs.exists(profile_dir .. "/workspace_teardown") then
        workspace_teardown = profile_dir .. "/workspace_teardown"
    elseif fs.exists(shared_dir .. "/workspace_teardown") then
        workspace_teardown = shared_dir .. "/workspace_teardown"
    end

    return {
        workspace_include = workspace_include,
        workspace_teardown = workspace_teardown,
        sessions = sessions,
    }
end

--- Check if shared config has an agent session with initialization.
-- When true, agents can be created without a profile (shared-only / "Default").
-- @param repo_root string Repository root path
-- @return boolean
function M.has_shared_agent(repo_root)
    if not repo_root then return false end
    return fs.exists(repo_root .. "/.botster/shared/sessions/agent/initialization")
end

--- List available profiles.
-- @param repo_root string Repository root path
-- @return string[] Profile names (directory names under .botster/profiles/)
function M.list_profiles(repo_root)
    if not repo_root then
        return {}
    end
    local profiles_dir = repo_root .. "/.botster/profiles"
    return list_subdirs(profiles_dir)
end

--- Get default template content for bootstrapping files.
-- @param config_type string "workspace_include"|"workspace_teardown"|"initialization"
-- @return string
function M.default_template(config_type)
    if config_type == "workspace_include" then
        return [[# Glob patterns for files to copy into agent worktrees.
# One pattern per line. Lines starting with # are comments.
#
# Examples:
#   .env
#   config/database.yml
#   node_modules/**
]]
    elseif config_type == "workspace_teardown" then
        return [[#!/bin/bash
# Cleanup commands run before worktree deletion.
# Available environment variables:
#   $BOTSTER_REPO            - owner/repo
#   $BOTSTER_ISSUE_NUMBER    - issue number (if applicable)
#   $BOTSTER_BRANCH_NAME     - branch name
#   $BOTSTER_WORKTREE_PATH   - worktree filesystem path
]]
    elseif config_type == "initialization" then
        return [[#!/bin/bash
# Session initialization script.
# This runs when the session starts inside the worktree.
]]
    else
        return ""
    end
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
