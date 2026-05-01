-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/home.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local M = {}

local function run_badge(run)
    if not run then
        return view.badge("ready", "muted")
    end
    if run.status == "done" then
        return view.badge("complete", "success")
    end
    if run.status == "blocked" then
        return view.badge("blocked", "danger")
    end
    return view.badge("in progress", "accent")
end

local function render_ticket(ticket, ctx)
    local runs = repo.ticket_runs(ticket.id)
    local latest = runs[1]
    local current_step = latest and latest.current_step_id and repo.get_step(latest.current_step_id) or nil
    local notifications = view.ticket_notification_count(ticket.id, repo)
    local tail = latest and ui.text{ text = current_step and ("Working: " .. current_step.name) or "In pipeline", size = "xs", tone = "muted" }
        or ui.text{ text = "No runs yet", size = "xs", tone = "muted" }

    return view.panel{
        ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.text{ text = ticket.title, size = "sm", weight = "semibold" },
                run_badge(latest),
                notifications > 0 and view.badge(tostring(notifications) .. " notification", "danger") or view.badge(view.target_label(ticket.target_id, ticket.target_path), "muted"),
            },
            ui.text{ text = ticket.description or "", size = "xs", tone = "muted" },
            view.row{
                ui.text{ text = string.format("%d run%s", #runs, #runs == 1 and "" or "s"), size = "xs", tone = "muted" },
                tail,
                ui.button{
                    label = "Open ticket",
                    icon = "arrow-right",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/tickets/" .. ticket.id) }),
                },
            },
        } },
    }
end

local function render_pipeline_summary(pipeline, ctx)
    local steps = repo.pipeline_steps(pipeline.id)
    local children = {
        view.row{
            ui.text{ text = pipeline.name, size = "sm", weight = "semibold" },
            ui.button{
                label = "Edit",
                icon = "pencil-square",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.nav.open", {
                    path = ctx.path("/pipelines/" .. pipeline.id .. "/edit"),
                }),
            },
        },
        ui.text{ text = pipeline.description or "", size = "xs", tone = "muted" },
        ui.text{ text = string.format("%d step%s", #steps, #steps == 1 and "" or "s"), size = "xs", tone = "muted" },
    }
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = children } }
end

function M.render(_view_state, ctx)
    local tickets = view.visible_tickets(repo)
    local runs = repo.list_runs(8)
    local pipelines = repo.list_pipelines()
    local projects = repo.list_projects()

    local ticket_nodes = {}
    if #tickets == 0 then
        table.insert(ticket_nodes, ui.text{
            text = "No tickets yet.",
            size = "sm",
            tone = "muted",
        })
    else
        for _, ticket in ipairs(tickets) do
            table.insert(ticket_nodes, render_ticket(ticket, ctx))
        end
    end

    local run_nodes = {}
    if #runs == 0 then
        table.insert(run_nodes, ui.text{ text = "No active runs.", size = "sm", tone = "muted" })
    else
        for _, run in ipairs(runs) do
            local ticket = repo.get_ticket(run.ticket_id)
            local pipeline = repo.get_pipeline(run.pipeline_id)
            table.insert(run_nodes, ui.button{
                label = (ticket and ticket.title or run.id) .. " - " .. (pipeline and pipeline.name or run.pipeline_id) .. " (" .. run.status .. ")",
                icon = "queue-list",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ctx.path("/runs/" .. run.id) }),
            })
        end
    end

    local pipeline_nodes = {}
    if #pipelines == 0 then
        table.insert(pipeline_nodes, ui.text{ text = "No pipelines yet. Create one with a project pipeline agent.", size = "sm", tone = "muted" })
    else
        for _, pipeline in ipairs(pipelines) do
            table.insert(pipeline_nodes, render_pipeline_summary(pipeline, ctx))
        end
    end

    local project_nodes = {}
    if #projects == 0 then
        table.insert(project_nodes, ui.text{ text = "No projects yet. Projects are optional and useful for multi-phase or coordinated work.", size = "sm", tone = "muted" })
    else
        for _, project in ipairs(projects) do
            table.insert(project_nodes, view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        ui.text{ text = project.name, size = "sm", weight = "semibold" },
                        view.badge(project.status),
                    },
                    ui.text{ text = project.description or "", size = "xs", tone = "muted" },
                    ui.button{
                        label = "Open project",
                        icon = "folder-open",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. project.id) }),
                    },
                } },
            })
        end
    end

    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.text{ text = "Project Pipelines", size = "lg", weight = "semibold" },
                ui.button{
                    label = "New ticket",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/new-ticket") }),
                },
                ui.button{
                    label = "New project",
                    icon = "folder-plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/new-project") }),
                },
                ui.button{
                    label = "Pipeline index",
                    icon = "queue-list",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/pipelines") }),
                },
            },
            ui.text{
                text = "Pipeline state, gate prompts, reviews, findings, artifacts, and plugin-owned sessions are persisted here.",
                size = "sm",
                tone = "muted",
            },
        } } },
        view.panel{ view.section("Projects", project_nodes) },
        view.panel{ view.section("Tickets", ticket_nodes) },
        view.panel{ view.section("Recent Runs", run_nodes) },
        view.section("Pipeline Definitions", pipeline_nodes),
    } }
end

return M
