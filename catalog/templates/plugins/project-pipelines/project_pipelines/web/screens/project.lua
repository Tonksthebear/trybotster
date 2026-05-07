-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/project.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local M = {}
local PERF = os.getenv("BOTSTER_LUA_PERF") == "1"

local function elapsed_ms(started)
    return math.floor(((os.clock() - started) * 1000) + 0.5)
end

local function log_perf(message)
    if PERF and log and log.info then
        log.info("[PERF][project_pipelines.project] " .. message)
    end
end

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

local function latest_run_badge(ticket, latest_run_by_ticket)
    local run = latest_run_by_ticket and latest_run_by_ticket[ticket.id] or repo.latest_ticket_run(ticket.id)
    if run then
        return view.badge(run.status, view.status_tone(run.status))
    end
    return view.badge("not started", "muted")
end

local function ticket_status_badge(ticket, open_run_by_ticket)
    if ticket.status == "closed" then
        return view.badge("closed", "success")
    end
    local open_run = open_run_by_ticket and open_run_by_ticket[ticket.id] or repo.open_ticket_run(ticket.id)
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

local function dependency_levels(tickets, dependencies_by_ticket)
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
        local dependencies = dependencies_by_ticket and dependencies_by_ticket[ticket_id] or repo.ticket_dependencies(ticket_id)
        for _, dependency in ipairs(dependencies or {}) do
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

local function dependency_tree_nodes(project_id, ctx, overview)
    local nodes = {}
    local tickets = overview and overview.tickets or sorted_project_tickets(project_id)
    local dependencies_by_ticket = overview and overview.dependencies_by_ticket or nil
    local levels = dependency_levels(tickets, dependencies_by_ticket)
    table.sort(tickets, function(a, b)
        local level_a = levels[a.id] or 0
        local level_b = levels[b.id] or 0
        if level_a == level_b then
            return (tonumber(a.created_at or 0) or 0) < (tonumber(b.created_at or 0) or 0)
        end
        return level_a < level_b
    end)

    for _, ticket in ipairs(tickets) do
        local dependencies = dependencies_by_ticket and dependencies_by_ticket[ticket.id] or repo.ticket_dependencies(ticket.id)
        local dependency_labels = {}
        for _, dependency in ipairs(dependencies or {}) do
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
                    ticket_status_badge(ticket, overview and overview.open_run_by_ticket),
                    latest_run_badge(ticket, overview and overview.latest_run_by_ticket),
                },
                ui.text{ text = details, size = "xs", tone = "muted" },
                ui.button{
                    id = "project-" .. project_id .. "-tree-ticket-" .. ticket.id,
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
                        id = ui.bind("@/id"),
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
    local render_started = PERF and os.clock() or nil
    local params = view_state and view_state.params or {}
    local started = PERF and os.clock() or nil
    local project = repo.get_project(params.project_id)
    if started then
        log_perf(string.format(
            "phase=get_project project_id=%s elapsed_ms=%d",
            tostring(params.project_id),
            elapsed_ms(started)))
    end
    if not project then
        return view.panel{ ui.text{ text = "Project not found", tone = "danger" } }
    end
    started = PERF and os.clock() or nil
    local overview = repo.project_dependency_overview(project.id)
    if started then
        log_perf(string.format(
            "phase=dependency_overview project_id=%s tickets=%d elapsed_ms=%d",
            tostring(project.id),
            #(overview and overview.tickets or {}),
            elapsed_ms(started)))
    end

    started = PERF and os.clock() or nil
    local target_nodes = project_target_nodes(project.id)
    if started then
        log_perf(string.format(
            "phase=project_targets project_id=%s nodes=%d elapsed_ms=%d",
            tostring(project.id),
            #target_nodes,
            elapsed_ms(started)))
    end

    started = PERF and os.clock() or nil
    local dependency_nodes = dependency_tree_nodes(project.id, ctx, overview)
    if started then
        log_perf(string.format(
            "phase=dependency_tree project_id=%s nodes=%d elapsed_ms=%d",
            tostring(project.id),
            #dependency_nodes,
            elapsed_ms(started)))
    end

    local timeline = timeline_nodes(project.id, ctx)

    local node = ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = project.name,
            back_id = "project-" .. project.id .. "-back",
            back_path = ctx.path("/"),
            meta = { view.badge(project.status) },
            actions = {
                ui.button{
                    id = "project-" .. project.id .. "-new-ticket",
                    label = "New ticket",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. project.id .. "/new-ticket") }),
                },
            },
            description = project.description or "",
        },
        view.section("Project Targets", target_nodes),
        view.section("Dependency Tree", dependency_nodes),
        view.section("Chronological Timeline", timeline),
    } }
    if render_started then
        log_perf(string.format(
            "phase=render_total project_id=%s elapsed_ms=%d",
            tostring(project.id),
            elapsed_ms(render_started)))
    end
    return node
end

return M
