local Agent = require("lib.agent")
local SessionClosePolicy = require("lib.session_close_policy")

local M = {}

local function shallow_copy(source)
    local copy = {}
    if type(source) ~= "table" then
        return copy
    end
    for k, v in pairs(source) do
        copy[k] = v
    end
    return copy
end

local function intrinsic_info(subject)
    if type(subject) ~= "table" then
        return {}
    end

    if type(subject.info) == "function" then
        return subject:info()
    end

    return subject
end

function M.build(subject, sessions)
    local info = shallow_copy(intrinsic_info(subject))
    local session_list = sessions
    if type(session_list) ~= "table" then
        session_list = Agent.all_info()
    end

    info.close_actions = SessionClosePolicy.close_actions_for_session(info, session_list)
    return info
end

function M.build_many(subjects)
    local list = {}
    local intrinsic = {}

    for _, subject in ipairs(subjects or {}) do
        intrinsic[#intrinsic + 1] = intrinsic_info(subject)
    end

    for _, info in ipairs(intrinsic) do
        list[#list + 1] = M.build(info, intrinsic)
    end

    return list
end

return M
