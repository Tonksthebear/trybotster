-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/surface.lua
-- @scope device
-- @version 1.0.0

local surfaces = require("lib.surfaces")
local actions = require("project_pipelines.web.actions")
local screen_home = require("project_pipelines.web.screens.home")
local screen_new = require("project_pipelines.web.screens.new")
local screen_project = require("project_pipelines.web.screens.project")
local screen_pipelines = require("project_pipelines.web.screens.pipelines")
local screen_run = require("project_pipelines.web.screens.run")
local screen_ticket = require("project_pipelines.web.screens.ticket")
local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local SURFACE = "pipelines"
local SIDEBAR = "pipelines_sidebar"
local OWNER = "project-pipelines"

local M = {}

local function render_session(view_state, ctx)
    local params = view_state and view_state.params or {}
    return ui.session_terminal{ session_uuid = params.session_uuid, back = ctx.path("/") }
end

local function has_open_questions()
    return #repo.open_questions() > 0
end

local function sidebar_button(attrs)
    local row = {
        ui.button{
            id = attrs.id,
            label = attrs.label,
            icon = attrs.icon,
            variant = attrs.variant or "ghost",
            tone = attrs.tone,
            action = ui.action("botster.nav.open", { path = attrs.path }),
        },
    }
    if attrs.badge then
        table.insert(row, attrs.badge)
    end
    if attrs.status then
        table.insert(row, ui.status_dot{ state = attrs.status, label = attrs.status_label or attrs.status })
    end
    return view.metadata(row)
end

local function render_sidebar()
    local children = {
        sidebar_button{
            id = "pipelines-sidebar-overview",
            label = "Overview",
            icon = "home",
            path = "/pipelines",
        },
        sidebar_button{
            id = "pipelines-sidebar-new-ticket",
            label = "New ticket",
            icon = "plus",
            variant = "solid",
            tone = "accent",
            path = "/pipelines/new-ticket",
        },
        sidebar_button{
            id = "pipelines-sidebar-new-project",
            label = "New project",
            icon = "folder-plus",
            path = "/pipelines/new-project",
        },
        sidebar_button{
            id = "pipelines-sidebar-pipelines",
            label = "Pipelines",
            icon = "queue-list",
            path = "/pipelines/pipelines",
        },
    }
    local projects = repo.list_projects()
    if #projects > 0 then
        table.insert(children, ui.text{ text = "Projects", size = "xs", weight = "semibold", tone = "muted" })
        for _, project in ipairs(projects) do
            table.insert(children, sidebar_button{
                id = "pipelines-sidebar-project-" .. project.id,
                label = project.name,
                icon = "folder",
                path = "/pipelines/projects/" .. project.id,
                badge = view.badge(project.status),
            })
        end
    end
    local questions = repo.open_questions()
    if #questions > 0 then
        table.insert(children, ui.text{ text = "Questions", size = "xs", weight = "semibold", tone = "muted" })
        for _, question in ipairs(questions) do
            local ticket = repo.get_ticket(question.ticket_id)
            table.insert(children, sidebar_button{
                id = "pipelines-sidebar-question-" .. question.id,
                label = ticket and ticket.title or question.ticket_id,
                icon = question.kind == "agent" and "user-circle" or "question-mark-circle",
                path = "/pipelines/tickets/" .. question.ticket_id,
                badge = view.badge(question.blocking == 1 and "blocking" or "open", question.blocking == 1 and "danger" or "accent"),
            })
        end
    end
    table.insert(children, ui.text{ text = "Tickets", size = "xs", weight = "semibold", tone = "muted" })
    for _, ticket in ipairs(view.visible_tickets(repo)) do
        local notifications = view.ticket_notification_count(ticket.id, repo)
        table.insert(children, sidebar_button{
            id = "pipelines-sidebar-ticket-" .. ticket.id,
            label = ticket.title,
            icon = notifications > 0 and "exclamation-circle" or "ticket",
            path = "/pipelines/tickets/" .. ticket.id,
            badge = view.notification_badge(notifications),
        })
    end

    return ui.stack{ direction = "vertical", gap = "3", children = children }
end

function M.register()
    surfaces.register(SURFACE, {
        label = "Pipelines",
        icon = "queue-list",
        nav = { section = "workspace", order = 30 },
        notification = has_open_questions,
        sidebar = { surface = SIDEBAR },
        source = "plugin:project-pipelines",
        routes = {
            { path = "/", render = screen_home.render },
            { path = "/new-ticket", render = screen_new.ticket },
            { path = "/new-project", render = screen_new.project },
            { path = "/projects/:project_id", render = screen_project.render },
            { path = "/projects/:project_id/new-ticket", render = screen_new.project_ticket },
            { path = "/tickets/:ticket_id", render = screen_ticket.render },
            { path = "/tickets/:ticket_id/sessions/:session_uuid", layout = "fullscreen", render = screen_ticket.session },
            { path = "/pipelines", render = screen_pipelines.index },
            { path = "/pipelines/:pipeline_id/edit", render = screen_pipelines.edit },
            { path = "/runs/:run_id", render = screen_run.render },
            { path = "/sessions/:session_uuid", layout = "fullscreen", render = render_session },
        },
    })

    surfaces.register(SIDEBAR, {
        source = "plugin:project-pipelines",
        render = render_sidebar,
    })

    actions.register()
end

return M
