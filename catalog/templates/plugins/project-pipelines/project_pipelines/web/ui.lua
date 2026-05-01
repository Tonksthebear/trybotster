-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/ui.lua
-- @scope device
-- @version 1.0.0

local M = {}
local ConfigResolver = require("lib.config_resolver")
local Agent = require("lib.agent")

function M.status_tone(status)
    if status == "done" or status == "approved" or status == "passed" or status == "resolved" then
        return "success"
    end
    if status == "blocked" or status == "failed" or status == "blocker" or status == "high" then
        return "danger"
    end
    if status == "active" then
        return "accent"
    end
    return "muted"
end

function M.badge(label, tone)
    return ui.badge{ text = tostring(label or ""), tone = tone or M.status_tone(label) }
end

function M.panel(children)
    return ui.panel{ tone = "default", border = true, children = children }
end

function M.row(children)
    return ui.inline{ gap = "2", align = "center", children = children }
end

function M.section(title, children)
    local nodes = { ui.text{ text = title, size = "md", weight = "semibold" } }
    for _, child in ipairs(children or {}) do
        table.insert(nodes, child)
    end
    return ui.stack{ direction = "vertical", gap = "2", children = nodes }
end

function M.field_action(id, payload)
    return ui.action(id, payload or {})
end

function M.agent_options(current)
    local device_root = nil
    if config and config.data_dir then
        local ok, data_dir = pcall(config.data_dir)
        if ok and data_dir then
            device_root = data_dir
        end
    end

    local seen = {}
    local options = {}
    local function add(name)
        if name and name ~= "" and not seen[name] then
            seen[name] = true
            table.insert(options, { value = name, label = name })
        end
    end

    add(current)
    for _, name in ipairs(ConfigResolver.list_agents(device_root, nil)) do
        add(name)
    end
    add("codex")
    add("claude")

    table.sort(options, function(a, b) return a.value < b.value end)
    return options
end

function M.targets()
    local targets = {}
    if spawn_targets and spawn_targets.list then
        for _, target in ipairs(spawn_targets.list() or {}) do
            if target.enabled ~= false then
                table.insert(targets, target)
            end
        end
    end
    table.sort(targets, function(a, b)
        return tostring(a.name or a.id) < tostring(b.name or b.id)
    end)
    return targets
end

function M.target_options(current_target_id)
    local options = {}
    local seen = {}
    local function add(value, label)
        if value and value ~= "" and not seen[value] then
            seen[value] = true
            table.insert(options, { value = value, label = label or value })
        end
    end
    for _, target in ipairs(M.targets()) do
        add(target.id, target.name or target.id)
    end
    if current_target_id then
        add(current_target_id, current_target_id)
    end
    return options
end

function M.target_by_id(target_id)
    for _, target in ipairs(M.targets()) do
        if target.id == target_id then
            return target
        end
    end
    return nil
end

function M.target_label(target_id, target_path)
    local target = M.target_by_id(target_id)
    if target then
        return target.name or target.id
    end
    return target_id or target_path or "No target"
end

function M.session_info(session_uuid)
    if not session_uuid or session_uuid == "" then
        return nil
    end
    local session = Agent.get(session_uuid)
    if session and session.info then
        local ok, info = pcall(session.info, session)
        if ok then
            return info
        end
    end
    return nil
end

function M.session_has_notification(session_uuid)
    local info = M.session_info(session_uuid)
    return info and info.notification == true
end

function M.ticket_notification_count(ticket_id, repo)
    local count = 0
    local seen = {}
    if not repo or not ticket_id then
        return 0
    end
    for _, uuid in ipairs(repo.ticket_session_uuids(ticket_id)) do
        if uuid and uuid ~= "" and not seen[uuid] then
            seen[uuid] = true
            if M.session_has_notification(uuid) then
                count = count + 1
            end
        end
    end
    return count
end

function M.ticket_should_show(ticket, repo)
    if not ticket then
        return false
    end
    if ticket.status ~= "closed" then
        return true
    end
    return M.ticket_notification_count(ticket.id, repo) > 0
end

function M.visible_tickets(repo)
    local tickets = {}
    if not repo then
        return tickets
    end
    local source = repo.standalone_tickets and repo.standalone_tickets() or repo.list_tickets()
    for _, ticket in ipairs(source) do
        if M.ticket_should_show(ticket, repo) then
            table.insert(tickets, ticket)
        end
    end
    return tickets
end

return M
