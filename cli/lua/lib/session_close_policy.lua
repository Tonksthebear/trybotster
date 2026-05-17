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

local function default_actions()
    return {
        can_close = true,
        can_delete_worktree = false,
        delete_worktree_reason = nil,
        other_active_sessions = 0,
    }
end

local function same_session(a, b)
    local a_id = session_id(a)
    local b_id = session_id(b)
    if a_id == nil and b_id == nil then
        return true
    end
    return a_id == b_id
end

local function non_empty(value)
    return type(value) == "string" and value ~= ""
end

local function append_index(index, key, entry)
    if key == nil then return end
    local bucket = index[key]
    if not bucket then
        bucket = {}
        index[key] = bucket
    end
    bucket[#bucket + 1] = entry
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
    local actions = default_actions()

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

function M.close_actions_for_sessions(sessions)
    local by_worktree_path = {}
    local by_workspace_id = {}

    for _, session in ipairs(sessions or {}) do
        if not system_session(session) then
            local path = worktree_path(session)
            if non_empty(path) then
                append_index(by_worktree_path, path, session)
            end
            append_index(by_workspace_id, workspace_id(session), session)
        end
    end

    local actions_by_session_id = {}
    local actions_by_subject = {}

    for _, target in ipairs(sessions or {}) do
        local actions = default_actions()

        if type(target) ~= "table" then
            actions.can_close = false
            actions.delete_worktree_reason = "session_missing"
        elseif not in_worktree(target) then
            actions.delete_worktree_reason = "not_in_worktree"
        else
            local seen = {}
            local other_count = 0

            local function count_bucket(bucket)
                for _, other in ipairs(bucket or {}) do
                    local key = session_id(other) or other
                    if not seen[key] and not same_session(target, other) then
                        seen[key] = true
                        other_count = other_count + 1
                    end
                end
            end

            local target_worktree_path = worktree_path(target)
            if non_empty(target_worktree_path) then
                count_bucket(by_worktree_path[target_worktree_path])
            end

            local target_workspace_id = workspace_id(target)
            if target_workspace_id ~= nil then
                count_bucket(by_workspace_id[target_workspace_id])
            end

            actions.other_active_sessions = other_count
            if other_count > 0 then
                actions.delete_worktree_reason = "other_sessions_active"
            else
                actions.can_delete_worktree = true
            end
        end

        local id = session_id(target)
        if id ~= nil then
            actions_by_session_id[id] = actions
        else
            actions_by_subject[target] = actions
        end
    end

    return {
        by_session_id = actions_by_session_id,
        by_subject = actions_by_subject,
    }
end

return M
