-- Generic plugin-owned actions for Botster sessions.
--
-- Plugins register action definitions once and derive per-session availability
-- from the current session record. Clients consume the derived records through
-- the `session_action` entity type and invoke them via
-- `execute_session_action`.

local M = {}

local registry = {}
local published_by_id = {}

local function copy_payload(source)
    local out = {}
    if type(source) ~= "table" then return out end
    for k, v in pairs(source) do out[k] = v end
    return out
end

local function call_or_value(value, session, action_id)
    if type(value) == "function" then
        return value(session, action_id)
    end
    return value
end

local function entity_id(session_uuid, action_id)
    return tostring(session_uuid) .. ":" .. tostring(action_id)
end

local function normalize_visibility(value)
    if value == nil or value == true then return "visible" end
    if value == false then return "hidden" end
    return tostring(value)
end

local function same_value(a, b)
    if a == b then return true end
    if type(a) ~= "table" or type(b) ~= "table" then return false end
    for k, v in pairs(a) do
        if not same_value(v, b[k]) then return false end
    end
    for k in pairs(b) do
        if a[k] == nil then return false end
    end
    return true
end

local function derive_action(session, action_id, entry)
    if type(session) ~= "table" or not session.session_uuid then return nil end

    local visible = normalize_visibility(call_or_value(entry.visibility, session, action_id))
    local enabled = call_or_value(entry.enabled, session, action_id)
    if enabled == nil then enabled = true end

    local status = call_or_value(entry.status, session, action_id)
    local icon = call_or_value(entry.icon, session, action_id)
    local label = call_or_value(entry.label, session, action_id) or action_id

    local action = {
        id = entity_id(session.session_uuid, action_id),
        session_uuid = session.session_uuid,
        action_id = action_id,
        label = label,
        status = status,
        icon = icon,
        visibility = visible,
        enabled = not not enabled,
        plugin = entry.plugin,
    }
    for _, field in ipairs(entry.descriptor_fields) do
        local value = call_or_value(field.value, session, action_id)
        if value ~= nil then action[field.name] = value end
    end
    return action
end

local function current_session(session_uuid)
    local Session = require("lib.session")
    local session = Session.get(session_uuid)
    if session and type(session.info) == "function" then
        return session:info()
    end
    return session
end

--- Register or replace a session action.
-- @param action_id string Stable plugin action id.
-- @param opts table {
--   run = function(session_uuid, action_id, context),
--   label/status/icon/visibility/enabled = value|function(session, action_id),
--   url/link_url/install_url/error = value|function(session, action_id),
--   plugin = string?,
-- }
function M.register(action_id, opts)
    assert(type(action_id) == "string" and action_id ~= "",
        "session_actions.register: action_id must be a non-empty string")
    assert(type(opts) == "table", "session_actions.register: opts table required")
    assert(type(opts.run) == "function", "session_actions.register: opts.run function required")

    local reserved = {
        run = true,
        label = true,
        status = true,
        icon = true,
        visibility = true,
        enabled = true,
        plugin = true,
    }
    local descriptor_fields = {}
    for key, value in pairs(opts) do
        if not reserved[key] and type(key) == "string" then
            descriptor_fields[#descriptor_fields + 1] = { name = key, value = value }
        end
    end
    table.sort(descriptor_fields, function(a, b) return a.name < b.name end)

    registry[action_id] = {
        run = opts.run,
        label = opts.label,
        status = opts.status,
        icon = opts.icon,
        visibility = opts.visibility,
        enabled = opts.enabled,
        plugin = opts.plugin,
        descriptor_fields = descriptor_fields,
    }

    M.publish_all_for_action(action_id)
end

function M.unregister(action_id)
    if not registry[action_id] then return false end
    registry[action_id] = nil

    local Session = require("lib.session")
    local EntityModel = require("lib.entity_model")
    for _, session in ipairs(Session.all_info()) do
        EntityModel.remove_session_action(session.session_uuid, action_id)
        published_by_id[entity_id(session.session_uuid, action_id)] = nil
    end
    return true
end

function M.get(action_id)
    return registry[action_id]
end

function M.action_ids()
    local ids = {}
    for action_id in pairs(registry) do ids[#ids + 1] = action_id end
    table.sort(ids)
    return ids
end

function M.entity_id(session_uuid, action_id)
    return entity_id(session_uuid, action_id)
end

function M.purge_session(session_uuid)
    if not session_uuid then return end
    local prefix = tostring(session_uuid) .. ":"
    for id in pairs(published_by_id) do
        if id:sub(1, #prefix) == prefix then
            published_by_id[id] = nil
        end
    end
end

function M.actions_for_session(session)
    local info = session
    if type(info) == "table" and type(info.info) == "function" then
        info = info:info()
    end

    local out = {}
    for action_id, entry in pairs(registry) do
        local action = derive_action(info, action_id, entry)
        if action then out[#out + 1] = action end
    end
    table.sort(out, function(a, b) return a.id < b.id end)
    return out
end

function M.all()
    local Session = require("lib.session")
    local out = {}
    for _, session in ipairs(Session.all_info()) do
        for _, action in ipairs(M.actions_for_session(session)) do
            out[#out + 1] = action
        end
    end
    table.sort(out, function(a, b) return a.id < b.id end)
    return out
end

function M.publish_for_session(session)
    local EntityModel = require("lib.entity_model")
    for _, action in ipairs(M.actions_for_session(session)) do
        if not same_value(published_by_id[action.id], action) then
            EntityModel.upsert_session_action(action)
            published_by_id[action.id] = action
        end
    end
end

function M.publish_all_for_action(action_id)
    local entry = registry[action_id]
    if not entry then return end

    local Session = require("lib.session")
    local EntityModel = require("lib.entity_model")
    for _, session in ipairs(Session.all_info()) do
        local action = derive_action(session, action_id, entry)
        if action and not same_value(published_by_id[action.id], action) then
            EntityModel.upsert_session_action(action)
            published_by_id[action.id] = action
        end
    end
end

--- Invoke a registered session action. The handler is responsible for queuing
--- any long-running work and returning promptly.
function M.run(session_uuid, action_id, context)
    if type(session_uuid) ~= "string" or session_uuid == "" then
        return nil, "session_uuid is required"
    end
    if type(action_id) ~= "string" or action_id == "" then
        return nil, "action_id is required"
    end

    local entry = registry[action_id]
    if not entry then
        return nil, "session action not registered: " .. action_id
    end

    local session = current_session(session_uuid)
    if not session then
        return nil, "session not found: " .. session_uuid
    end

    local action = derive_action(session, action_id, entry)
    if action.visibility == "hidden" then
        return nil, "session action is not visible: " .. action_id
    end
    if not action.enabled then
        return nil, "session action is disabled: " .. action_id
    end

    local payload = copy_payload(context)
    payload.session = session
    payload.action = action
    local ok, result, err = pcall(entry.run, session_uuid, action_id, payload)
    if not ok then
        return nil, result
    end
    if (result == nil or result == false) and err ~= nil then
        return nil, err
    end
    return true, result
end

function M._reset_for_tests()
    for k in pairs(registry) do registry[k] = nil end
    for k in pairs(published_by_id) do published_by_id[k] = nil end
end

return M
