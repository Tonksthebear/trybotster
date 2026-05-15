-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/notification_policy.lua
-- @scope device
-- @version 1.1.0

local M = {}
local SURFACE = "pipelines"

M.rules = {
    { id = "permission", label = "permission prompts", keywords = { "permission" } },
    { id = "approval_requested", label = "approval requests", keywords = { "approval requested" } },
    { id = "wants_to_edit", label = "edit approval prompts", keywords = { "wants to edit" } },
}

local function append_value(parts, value)
    if value == nil then
        return
    end
    if type(value) == "table" then
        for _, item in ipairs(value) do
            append_value(parts, item)
        end
        for key, item in pairs(value) do
            if type(key) ~= "number" then
                append_value(parts, item)
            end
        end
        return
    end
    parts[#parts + 1] = tostring(value)
end

local function notification_text(intent)
    intent = intent or {}
    local parts = {}
    append_value(parts, intent.message)
    append_value(parts, intent.title)
    append_value(parts, intent.body)
    append_value(parts, intent.text)
    append_value(parts, intent.summary)
    append_value(parts, intent.keywords)
    return table.concat(parts, "\n")
end

local function matching_rule(intent)
    local text = notification_text(intent):lower()
    for _, rule in ipairs(M.rules) do
        local keywords = rule.keywords or { rule.pattern }
        for _, keyword in ipairs(keywords) do
            local normalized = tostring(keyword or ""):lower()
            if normalized ~= "" and text:find(normalized, 1, true) then
                return rule, keyword
            end
        end
    end
    return nil
end

local function log_suppressed(intent)
    if type(log) ~= "table" or type(log.info) ~= "function" then
        return
    end
    local text = notification_text(intent)
    log.info(string.format(
        "[project-pipelines] notification suppressed session=%s type=%s text=%q",
        tostring(intent and intent.session_uuid or ""),
        tostring(intent and intent.type or ""),
        text))
end

function M.evaluate(intent)
    local rule = matching_rule(intent)
    if rule then
        return {
            core = "replace",
            reason = "project_pipelines_allowed_" .. rule.id,
            custom = {},
        }
    end

    log_suppressed(intent)
    return {
        core = "suppress",
        reason = "project_pipelines_routine_cli_notification",
    }
end

function M.register()
    local notifications = require("lib.notifications")
    notifications.claim({
        name = "project_pipelines.notification_policy",
        scope = { owner_plugin = "project-pipelines" },
        priority = 1000,
        handler = M.evaluate,
    })
end

local function surface_path(path, params)
    local ok, surfaces = pcall(require, "lib.surfaces")
    if not ok or type(surfaces) ~= "table" or type(surfaces.path) ~= "function" then
        return nil
    end
    return surfaces.path(SURFACE, path, params)
end

local function send_push(opts)
    if type(push) ~= "table" or type(push.send) ~= "function" then
        return
    end
    local ok, err = pcall(push.send, opts)
    if not ok then
        log.warn("[project-pipelines] notification push failed: " .. tostring(err))
    end
end

function M.notify_phase_transition(attrs)
    attrs = attrs or {}
    local ticket = attrs.ticket or {}
    local step = attrs.step
    local title = step and "Pipeline phase changed" or "Pipeline completed"
    local body = step
        and string.format("%s moved to %s.", tostring(ticket.title or attrs.ticket_id or "Ticket"), tostring(step.name or step.id or "next step"))
        or string.format("%s completed its pipeline.", tostring(ticket.title or attrs.ticket_id or "Ticket"))
    local url = attrs.ticket_id and surface_path("/tickets/:ticket_id", { ticket_id = attrs.ticket_id }) or nil

    send_push({
        kind = "project_pipelines_phase_transition",
        title = title,
        body = body,
        url = url,
        tag = attrs.run_id and ("project-pipelines:run:" .. tostring(attrs.run_id)) or nil,
    })
end

function M.notify_question_asked(attrs)
    attrs = attrs or {}
    local question = attrs.question or {}
    local ticket = attrs.ticket or {}
    local url = question.ticket_id and surface_path("/tickets/:ticket_id", { ticket_id = question.ticket_id }) or nil

    send_push({
        kind = "project_pipelines_question",
        title = "Pipeline question asked",
        body = tostring(ticket.title or question.ticket_id or "Ticket") .. ": " .. tostring(question.question or "Question needs an answer."),
        url = url,
        tag = question.id and ("project-pipelines:question:" .. tostring(question.id)) or nil,
    })
end

return M
