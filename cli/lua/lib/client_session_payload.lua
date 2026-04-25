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

-- Wire protocol — fields whose change requires re-deriving another
-- field on the client. When a `Session:update` patches a derived input
-- (title, agent_name, branch_name), include the derivation (display_name)
-- in the same patch so clients don't have to re-fetch.
local DERIVATION_INPUTS = {
    title = "display_name",
    agent_name = "display_name",
    branch_name = "display_name",
}

local function display_name_for(session)
    if type(session) ~= "table" then return nil end
    -- Mirrors the web/TUI displayName selector logic: label > display_name >
    -- title > session_uuid. Hub-side computation matters because patches go
    -- straight to the client store without re-running selectors.
    if type(session.label) == "string" and session.label ~= "" then
        return session.label
    end
    if type(session.display_name) == "string" and session.display_name ~= "" then
        return session.display_name
    end
    if type(session.title) == "string" and session.title ~= "" then
        return session.title
    end
    return session.session_uuid
end

--- Project a sparse `Session:update(...)` field set into the patch payload
--- the wire ships, including any re-derived fields. Per design brief §12.4:
---   * title         → { title, display_name }
---   * agent_name    → { agent_name, display_name }
---   * branch_name   → { branch_name, display_name }
---   * notification  → { notification }
---   * is_idle       → { is_idle }
---   * cwd           → { cwd }
---   * status        → { status }
---   * hosted_preview→ { hosted_preview = { ...whole nested object... } }
---
--- @param changed_fields table Sparse {field=value} table from Session:update.
--- @param session table The Session record AFTER applying changes (used to
---   compute derivations like display_name).
--- @return table {field=value} payload ready for EB.patch.
function M.project_fields(changed_fields, session)
    if type(changed_fields) ~= "table" then return {} end
    local out = {}
    local needs_display_name = false
    for k, v in pairs(changed_fields) do
        out[k] = v
        if DERIVATION_INPUTS[k] then
            needs_display_name = true
        end
    end
    if needs_display_name then
        out.display_name = display_name_for(session)
    end
    return out
end

return M
