-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/project.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local M = {}

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

local function ticket_nodes(project_id, ctx)
    local nodes = {}
    for _, ticket in ipairs(repo.project_tickets(project_id)) do
        if view.ticket_should_show(ticket, repo) then
            local run = repo.open_ticket_run(ticket.id)
            local notifications = view.ticket_notification_count(ticket.id, repo)
            table.insert(nodes, view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        ui.text{ text = ticket.title, size = "sm", weight = "semibold" },
                        run and view.badge(run.status == "blocked" and "blocked" or "in progress", run.status == "blocked" and "danger" or "accent") or view.badge("ready", "muted"),
                        notifications > 0 and view.badge(tostring(notifications) .. " notification", "danger") or view.badge(view.target_label(ticket.target_id, ticket.target_path), "accent"),
                    },
                    ui.text{ text = ticket.description or "", size = "xs", tone = "muted" },
                    ui.button{
                        label = "Open ticket",
                        icon = "arrow-right",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("botster.nav.open", { path = ctx.path("/tickets/" .. ticket.id) }),
                    },
                } },
            })
        end
    end
    if #nodes == 0 then
        table.insert(nodes, ui.text{ text = "No tickets in this project yet.", size = "sm", tone = "muted" })
    end
    return nodes
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
        view.section("Tickets", ticket_nodes(project.id, ctx)),
    } }
end

return M
