-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/new.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")
local actions = require("project_pipelines.web.actions")

local M = {}

local function project_options(selected_project_id)
    local options = {
        { value = "", label = "No project" },
    }
    for _, project in ipairs(repo.list_projects()) do
        table.insert(options, { value = project.id, label = project.name })
    end
    return options
end

function M.ticket(_view_state, ctx)
    local recent = {}
    for _, ticket in ipairs(view.visible_tickets(repo)) do
        local run = repo.open_ticket_run(ticket.id)
        table.insert(recent, view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    ui.text{ text = ticket.title, size = "sm", weight = "semibold" },
                    run and view.badge(run.status == "blocked" and "blocked" or "in progress", run.status == "blocked" and "danger" or "accent") or view.badge("ready", "muted"),
                    view.badge(view.target_label(ticket.target_id, ticket.target_path), "accent"),
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
        if #recent >= 3 then
            break
        end
    end
    if #recent == 0 then
        table.insert(recent, ui.text{ text = "No tickets yet.", size = "sm", tone = "muted" })
    end

    local children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.button{
                    label = "Back",
                    icon = "arrow-left",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/") }),
                },
                ui.text{ text = "New Ticket", size = "lg", weight = "semibold" },
            },
            ui.text{ text = "A ticket is one concrete unit of work in one spawn target.", size = "sm", tone = "muted" },
        } } },
    }
    table.insert(children, view.panel{ ui.form{ children = {
        ui.stack{ direction = "vertical", gap = "3", children = {
            ui.select{
                id = "new-ticket-target",
                label = "Spawn target",
                required = true,
                placeholder = "Select target",
                options = view.target_options(),
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "target_id" }),
            },
            ui.select{
                id = "new-ticket-project",
                label = "Project",
                value = "",
                options = project_options(),
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "project_id" }),
            },
            ui.text_input{
                id = "new-ticket-title",
                label = "Title",
                required = true,
                placeholder = "Ticket title",
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "title" }),
            },
            ui.textarea{
                id = "new-ticket-description",
                label = "Description",
                placeholder = "Describe the work",
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "description" }),
            },
            ui.button{
                label = "Create ticket",
                icon = "plus",
                variant = "solid",
                tone = "accent",
                action = ui.action("project_pipelines.create_ticket"),
            },
        } },
    } } })
    table.insert(children, view.section("Recent Tickets", recent))

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

function M.project_ticket(view_state, ctx)
    local params = view_state and view_state.params or {}
    local project = repo.get_project(params.project_id)
    local title = project and ("New Ticket - " .. project.name) or "New Ticket"

    local children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.button{
                    label = "Back",
                    icon = "arrow-left",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. params.project_id) }),
                },
                ui.text{ text = title, size = "lg", weight = "semibold" },
            },
            ui.text{ text = "A project ticket still belongs to one spawn target.", size = "sm", tone = "muted" },
        } } },
    }
    table.insert(children, view.panel{ ui.form{ children = {
        ui.stack{ direction = "vertical", gap = "3", children = {
            ui.select{
                id = "new-project-ticket-target",
                label = "Spawn target",
                required = true,
                placeholder = "Select target",
                options = view.target_options(),
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "target_id" }),
            },
            ui.select{
                id = "new-project-ticket-project",
                label = "Project",
                value = params.project_id or "",
                options = project_options(params.project_id),
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "project_id" }),
            },
            ui.text_input{
                id = "new-project-ticket-title",
                label = "Title",
                required = true,
                placeholder = "Ticket title",
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "title" }),
            },
            ui.textarea{
                id = "new-project-ticket-description",
                label = "Description",
                placeholder = "Describe the work",
                on_change = view.field_action("project_pipelines.update_ticket_draft", { field = "description" }),
            },
            ui.button{
                label = "Create ticket",
                icon = "plus",
                variant = "solid",
                tone = "accent",
                action = ui.action("project_pipelines.create_ticket", { project_id = params.project_id }),
            },
        } },
    } } })

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

function M.project(_view_state, ctx)
    local recent = {}
    for _, project in ipairs(repo.list_projects()) do
        table.insert(recent, view.panel{
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
        if #recent >= 3 then
            break
        end
    end
    if #recent == 0 then
        table.insert(recent, ui.text{ text = "No projects yet.", size = "sm", tone = "muted" })
    end
    local children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.button{
                    label = "Back",
                    icon = "arrow-left",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/") }),
                },
                ui.text{ text = "New Project", size = "lg", weight = "semibold" },
            },
            ui.text{ text = "A project coordinates multi-phase or cross-target work. Projects are optional.", size = "sm", tone = "muted" },
        } } },
    }
    table.insert(children, view.panel{ ui.form{ children = {
        ui.stack{ direction = "vertical", gap = "3", children = {
            ui.text_input{
                id = "new-project-name",
                label = "Name",
                required = true,
                placeholder = "Project name",
                on_change = view.field_action("project_pipelines.update_project_draft", { field = "name" }),
            },
            ui.textarea{
                id = "new-project-description",
                label = "Description",
                placeholder = "Shared goals, phases, or cross-target context",
                on_change = view.field_action("project_pipelines.update_project_draft", { field = "description" }),
            },
            ui.select{
                id = "new-project-target",
                label = "Optional spawn target",
                placeholder = "No default target",
                options = view.target_options(),
                on_change = view.field_action("project_pipelines.update_project_draft", { field = "target_id" }),
            },
            ui.button{
                label = "Create project",
                icon = "folder-plus",
                variant = "solid",
                tone = "accent",
                action = ui.action("project_pipelines.create_project"),
            },
        } },
    } } })
    table.insert(children, view.section("Recent Projects", recent))

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

return M
