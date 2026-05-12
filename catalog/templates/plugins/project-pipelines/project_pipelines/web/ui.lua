-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/ui.lua
-- @scope device
-- @version 1.0.0

local M = {}
local ConfigResolver = require("lib.config_resolver")
local Agent = require("lib.agent")

local targets_cache = {
    loaded_at = 0,
    values = nil,
    by_id = nil,
}

local session_info_cache = {
    loaded_at = 0,
    by_uuid = {},
}

function M.status_tone(status)
    if status == "open" then
        return "accent"
    end
    if status == "closed" then
        return "success"
    end
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

function M.status_label(status)
    if type(status) == "table" then
        return status
    end
    if status == "open" then
        return "+"
    end
    if status == "closed" then
        return "✓"
    end
    return tostring(status or "")
end

function M.status_state(status)
    if status == "open" or status == "active" then
        return "active"
    end
    if status == "closed" or status == "done" or status == "approved" or status == "passed" or status == "resolved" then
        return "success"
    end
    if status == "blocked" or status == "failed" or status == "blocker" or status == "high" then
        return "danger"
    end
    return "neutral"
end

function M.badge(label, tone)
    return ui.badge{ text = M.status_label(label), tone = tone or M.status_tone(label) }
end

function M.status_mark(status)
    return ui.status_dot{ state = M.status_state(status), label = M.status_label(status) }
end

function M.panel(children)
    return ui.panel{ tone = "default", border = true, children = children }
end

function M.row(children)
    return ui.inline{ gap = "2", align = "center", wrap = true, children = children }
end

function M.responsive_row(children, opts)
    opts = opts or {}
    return ui.stack{
        direction = ui.responsive({ compact = "vertical", expanded = "horizontal" }),
        gap = opts.gap or "2",
        align = opts.align or ui.responsive({ compact = "stretch", expanded = "center" }),
        justify = opts.justify,
        children = children,
    }
end

function M.action_row(children)
    return M.responsive_row(children, { gap = "2", align = ui.responsive({ compact = "stretch", expanded = "center" }) })
end

function M.metadata(children)
    return ui.inline{ gap = "2", align = "center", wrap = true, children = children }
end

function M.section(title, children)
    local nodes = { ui.text{ text = title, size = "md", weight = "semibold" } }
    for _, child in ipairs(children or {}) do
        table.insert(nodes, child)
    end
    return ui.stack{ direction = "vertical", gap = "2", children = nodes }
end

function M.empty(title, description, icon)
    return ui.empty_state{
        title = title,
        description = description,
        icon = icon or "inbox",
    }
end

function M.action_button(attrs)
    return ui.button{
        id = attrs.id,
        label = attrs.label,
        icon = attrs.icon,
        variant = attrs.variant or "solid",
        tone = attrs.tone or "accent",
        action = attrs.action,
    }
end

function M.page_header(attrs)
    local start = {}
    if attrs.back_path then
        table.insert(start, ui.button{
            id = attrs.back_id,
            label = attrs.back_label or "Back",
            icon = "arrow-left",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = attrs.back_path }),
        })
    end
    table.insert(start, ui.text{ text = attrs.title or "", size = "lg", weight = "semibold" })
    for _, item in ipairs(attrs.meta or {}) do
        table.insert(start, item)
    end

    local children = {
        M.responsive_row({
            M.metadata(start),
            M.action_row(attrs.actions or {}),
        }, { gap = "3", align = ui.responsive({ compact = "stretch", expanded = "center" }), justify = "between" }),
    }
    if attrs.description and attrs.description ~= "" then
        table.insert(children, ui.text{ text = attrs.description, size = "sm", tone = "muted" })
    end
    return M.panel{ ui.stack{ direction = "vertical", gap = "2", children = children } }
end

function M.notification_badge(count)
    count = tonumber(count or 0) or 0
    if count <= 0 then
        return nil
    end
    return M.badge(tostring(count) .. " notification", "danger")
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

function M.accessory_options(current, target_path)
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
    for _, name in ipairs(ConfigResolver.list_accessories(device_root, target_path)) do
        add(name)
    end
    add("terminal")

    table.sort(options, function(a, b)
        if a.value == "terminal" then return true end
        if b.value == "terminal" then return false end
        return a.value < b.value
    end)
    return options
end

function M.targets()
    local now = os.time()
    if targets_cache.values and (now - targets_cache.loaded_at) <= 1 then
        return targets_cache.values
    end

    local targets = {}
    local by_id = {}
    if spawn_targets and spawn_targets.list then
        for _, target in ipairs(spawn_targets.list() or {}) do
            if target.enabled ~= false then
                table.insert(targets, target)
                if target.id then
                    by_id[target.id] = target
                end
            end
        end
    end
    table.sort(targets, function(a, b)
        return tostring(a.name or a.id) < tostring(b.name or b.id)
    end)
    targets_cache.loaded_at = now
    targets_cache.values = targets
    targets_cache.by_id = by_id
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
    M.targets()
    return targets_cache.by_id and targets_cache.by_id[target_id] or nil
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
    local now = os.time()
    if now - session_info_cache.loaded_at > 1 then
        session_info_cache.loaded_at = now
        session_info_cache.by_uuid = {}
    elseif session_info_cache.by_uuid[session_uuid] ~= nil then
        return session_info_cache.by_uuid[session_uuid] or nil
    end
    local session = Agent.get(session_uuid)
    if session and session.info then
        local ok, info = pcall(session.info, session)
        if ok then
            session_info_cache.by_uuid[session_uuid] = info or false
            return info
        end
    end
    session_info_cache.by_uuid[session_uuid] = false
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

function M.ticket_notification_counts(repo, ticket_ids)
    if not repo or not repo.ticket_session_uuids_by_ticket then
        return {}
    end
    local uuids_by_ticket, all_uuids = repo.ticket_session_uuids_by_ticket(ticket_ids)
    local notified = {}
    for uuid in pairs(all_uuids or {}) do
        notified[uuid] = M.session_has_notification(uuid) == true
    end
    local counts = {}
    for ticket_id, uuids in pairs(uuids_by_ticket or {}) do
        local count = 0
        for _, uuid in ipairs(uuids) do
            if notified[uuid] then
                count = count + 1
            end
        end
        counts[ticket_id] = count
    end
    return counts
end

function M.notification_count_for_uuids(uuids)
    local count = 0
    local seen = {}
    for _, uuid in ipairs(uuids or {}) do
        if uuid and uuid ~= "" and not seen[uuid] then
            seen[uuid] = true
            if M.session_has_notification(uuid) then
                count = count + 1
            end
        end
    end
    return count
end

function M.ticket_should_show(ticket, repo, notification_counts)
    if not ticket then
        return false
    end
    if ticket.status ~= "closed" then
        return true
    end
    if notification_counts then
        return tonumber(notification_counts[ticket.id] or 0) > 0
    end
    return M.ticket_notification_count(ticket.id, repo) > 0
end

function M.visible_tickets(repo, notification_counts)
    local tickets = {}
    if not repo then
        return tickets
    end
    for _, ticket in ipairs(repo.standalone_tickets()) do
        if M.ticket_should_show(ticket, repo, notification_counts) then
            table.insert(tickets, ticket)
        end
    end
    return tickets
end

return M
