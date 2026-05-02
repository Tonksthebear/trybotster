-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/project.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local M = {}

local function sorted_project_tickets(project_id)
    local tickets = repo.project_tickets(project_id)
    table.sort(tickets, function(a, b)
        local a_time = tonumber(a.created_at or 0) or 0
        local b_time = tonumber(b.created_at or 0) or 0
        if a_time == b_time then
            return tostring(a.id) < tostring(b.id)
        end
        return a_time < b_time
    end)
    return tickets
end

local function time_label(value)
    local timestamp = tonumber(value or 0) or 0
    if timestamp <= 0 then
        return ""
    end
    return os.date("%Y-%m-%d %H:%M", timestamp)
end

local function latest_run_badge(ticket)
    local run = repo.latest_ticket_run(ticket.id)
    if run then
        return view.badge(run.status, view.status_tone(run.status))
    end
    return view.badge("not started", "muted")
end

local function ticket_status_badge(ticket)
    if ticket.status == "closed" then
        return view.badge("closed", "success")
    end
    local open_run = repo.open_ticket_run(ticket.id)
    if open_run then
        return view.badge(open_run.status == "blocked" and "blocked" or "in progress", open_run.status == "blocked" and "danger" or "accent")
    end
    return view.badge(ticket.status or "open", "muted")
end

local function project_target_nodes(project_id)
    local nodes = {}
    for _, target in ipairs(repo.project_targets(project_id)) do
        table.insert(nodes, view.badge(view.target_label(target.target_id, target.target_path), "accent"))
    end
    if #nodes == 0 then
        table.insert(nodes, ui.text{ text = "No project-level targets.", size = "sm", tone = "muted" })
    end
    return nodes
end

local function dependency_levels(tickets)
    local by_id = {}
    for _, ticket in ipairs(tickets) do
        by_id[ticket.id] = ticket
    end

    local memo = {}
    local visiting = {}
    local function level(ticket_id)
        if memo[ticket_id] then
            return memo[ticket_id]
        end
        if visiting[ticket_id] then
            return 0
        end
        visiting[ticket_id] = true
        local max_dependency_level = -1
        for _, dependency in ipairs(repo.ticket_dependencies(ticket_id)) do
            if by_id[dependency.depends_on_ticket_id] then
                max_dependency_level = math.max(max_dependency_level, level(dependency.depends_on_ticket_id))
            end
        end
        visiting[ticket_id] = nil
        memo[ticket_id] = max_dependency_level + 1
        return memo[ticket_id]
    end

    for _, ticket in ipairs(tickets) do
        level(ticket.id)
    end
    return memo
end

local function dependency_tree_nodes(project_id, ctx)
    local nodes = {}
    local tickets = sorted_project_tickets(project_id)
    local levels = dependency_levels(tickets)
    table.sort(tickets, function(a, b)
        local level_a = levels[a.id] or 0
        local level_b = levels[b.id] or 0
        if level_a == level_b then
            return (tonumber(a.created_at or 0) or 0) < (tonumber(b.created_at or 0) or 0)
        end
        return level_a < level_b
    end)

    for _, ticket in ipairs(tickets) do
        local dependencies = repo.ticket_dependencies(ticket.id)
        local dependency_labels = {}
        for _, dependency in ipairs(dependencies) do
            table.insert(dependency_labels, dependency.depends_on_title or dependency.depends_on_ticket_id)
        end
        local level = levels[ticket.id] or 0
        local title = string.rep("  ", level) .. ticket.title
        local details = #dependency_labels > 0
            and ("Depends on: " .. table.concat(dependency_labels, ", "))
            or "Starts this branch of work."
        table.insert(nodes, view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge("stage " .. tostring(level + 1), "muted"),
                    ui.text{ text = title, size = "sm", weight = "semibold" },
                    ticket_status_badge(ticket),
                    latest_run_badge(ticket),
                },
                ui.text{ text = details, size = "xs", tone = "muted" },
                ui.button{
                    label = "Open ticket",
                    icon = "arrow-right",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/tickets/" .. ticket.id) }),
                },
            } },
        })
    end
    if #nodes == 0 then
        table.insert(nodes, ui.text{ text = "No tickets in this project yet.", size = "sm", tone = "muted" })
    end
    return nodes
end

local function timeline_nodes(project_id, ctx)
    return {
        ui.bind_list{
            source = "/project-pipelines.ticket",
            where = { project_id = project_id },
            item_template = view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
                    view.badge(ui.bind("@/status")),
                    view.badge(ui.bind("@/latest_run_badge"), ui.bind("@/latest_run_tone")),
                },
                ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
                view.row{
                    view.badge(ui.bind("@/target_label"), "accent"),
                    ui.button{
                        label = "Open",
                        icon = "arrow-right",
                        variant = "ghost",
                        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
                    },
                },
            } },
        },
        },
    }
end

function M.render(view_state, ctx)
    local params = view_state and view_state.params or {}
    local project = repo.get_project(params.project_id)
    if not project then
        return view.panel{ ui.text{ text = "Project not found", tone = "danger" } }
    end

    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.button{
                    label = "Back",
                    icon = "arrow-left",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/") }),
                },
                ui.text{ text = project.name, size = "lg", weight = "semibold" },
                view.badge(project.status),
                ui.button{
                    label = "New ticket",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. project.id .. "/new-ticket") }),
                },
            },
            ui.text{ text = project.description or "", size = "sm", tone = "muted" },
        } } },
        view.section("Project Targets", project_target_nodes(project.id)),
        view.section("Dependency Tree", dependency_tree_nodes(project.id, ctx)),
        view.section("Chronological Timeline", timeline_nodes(project.id, ctx)),
    } }
end

return M
