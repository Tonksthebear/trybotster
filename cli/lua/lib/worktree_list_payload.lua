local TargetContext = require("lib.target_context")

local M = {}

local function system_session(entry)
    if type(entry) ~= "table" then return false end

    if entry.system_session ~= nil then
        return entry.system_session == true or entry.system_session == "true"
    end

    local metadata = entry.metadata
    return type(metadata) == "table"
        and (metadata.system_session == true or metadata.system_session == "true")
end

local function in_worktree(entry)
    if type(entry) ~= "table" then return false end
    if entry.in_worktree ~= nil then return entry.in_worktree == true end
    if entry._is_worktree ~= nil then return entry._is_worktree == true end
    return false
end

local function mergeable_session(entry, target)
    if type(entry) ~= "table" then return false end
    if system_session(entry) or not in_worktree(entry) then return false end

    local worktree_path = entry.worktree_path
    if type(worktree_path) ~= "string" or worktree_path == "" then
        return false
    end

    if target and not TargetContext.matches(entry, target) then
        return false
    end

    return true
end

local function new_entry(path, branch)
    return {
        path = path,
        branch = branch or "",
        active_sessions = 0,
    }
end

function M.build(target, listed_worktrees, sessions)
    local merged = {}
    local by_path = {}

    for _, worktree in ipairs(listed_worktrees or {}) do
        local path = type(worktree) == "table" and worktree.path or nil
        if type(path) == "string" and path ~= "" and not by_path[path] then
            local entry = new_entry(path, worktree.branch)
            merged[#merged + 1] = entry
            by_path[path] = entry
        end
    end

    for _, session in ipairs(sessions or {}) do
        if mergeable_session(session, target) then
            local path = session.worktree_path
            local entry = by_path[path]
            if not entry then
                entry = new_entry(path, session.branch_name)
                merged[#merged + 1] = entry
                by_path[path] = entry
            elseif (not entry.branch or entry.branch == "") and session.branch_name then
                entry.branch = session.branch_name
            end

            entry.active_sessions = (entry.active_sessions or 0) + 1
        end
    end

    return merged
end

return M
