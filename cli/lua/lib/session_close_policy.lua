local Session = require("lib.session")

local M = {}

local function session_id(entry)
    if type(entry) ~= "table" then return nil end
    return entry.session_uuid or entry.id
end

local function workspace_id(entry)
    if type(entry) ~= "table" then return nil end
    return entry.workspace_id or entry._workspace_id
end

local function worktree_path(entry)
    if type(entry) ~= "table" then return nil end
    return entry.worktree_path
end

local function in_worktree(entry)
    if type(entry) ~= "table" then return false end
    if entry.in_worktree ~= nil then return entry.in_worktree == true end
    if entry._is_worktree ~= nil then return entry._is_worktree == true end
    return false
end

local function system_session(entry)
    if type(entry) ~= "table" then return false end

    if entry.system_session ~= nil then
        return entry.system_session == true or entry.system_session == "true"
    end

    local metadata = entry.metadata
    if type(metadata) == "table" and
        (metadata.system_session == true or metadata.system_session == "true") then
        return true
    end

    return Session.is_system_session(entry)
end

local function shares_removal_scope(target, other)
    local target_worktree_path = worktree_path(target)
    local other_worktree_path = worktree_path(other)
    if target_worktree_path ~= nil
        and target_worktree_path ~= ""
        and other_worktree_path ~= nil
        and other_worktree_path ~= ""
        and target_worktree_path == other_worktree_path then
        return true
    end

    local target_workspace_id = workspace_id(target)
    local other_workspace_id = workspace_id(other)
    if target_workspace_id and other_workspace_id then
        return target_workspace_id == other_workspace_id
    end

    return false
end

function M.other_active_sessions(target, sessions)
    local others = {}
    if type(target) ~= "table" then return others end

    for _, other in ipairs(sessions or {}) do
        if not system_session(other)
            and session_id(other) ~= session_id(target)
            and shares_removal_scope(target, other) then
            others[#others + 1] = other
        end
    end

    return others
end

function M.close_actions_for_session(target, sessions)
    local actions = {
        can_close = true,
        can_delete_worktree = false,
        delete_worktree_reason = nil,
        other_active_sessions = 0,
    }

    if type(target) ~= "table" then
        actions.can_close = false
        actions.delete_worktree_reason = "session_missing"
        return actions
    end

    if not in_worktree(target) then
        actions.delete_worktree_reason = "not_in_worktree"
        return actions
    end

    local others = M.other_active_sessions(target, sessions)
    actions.other_active_sessions = #others
    if #others > 0 then
        actions.delete_worktree_reason = "other_sessions_active"
        return actions
    end

    actions.can_delete_worktree = true
    return actions
end

return M
