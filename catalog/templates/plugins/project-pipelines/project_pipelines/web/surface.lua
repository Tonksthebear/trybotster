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

local function render_sidebar()
    local children = {
        ui.button{
            label = "Overview",
            icon = "home",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = "/pipelines" }),
        },
        ui.button{
            label = "New ticket",
            icon = "plus",
            variant = "solid",
            tone = "accent",
            action = ui.action("botster.nav.open", { path = "/pipelines/new-ticket" }),
        },
        ui.button{
            label = "New project",
            icon = "folder-plus",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = "/pipelines/new-project" }),
        },
        ui.button{
            label = "Pipelines",
            icon = "queue-list",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = "/pipelines/pipelines" }),
        },
    }
    local projects = repo.list_projects()
    if #projects > 0 then
        table.insert(children, ui.text{ text = "Projects", size = "xs", weight = "semibold", tone = "muted" })
        for _, project in ipairs(projects) do
            table.insert(children, ui.button{
                label = project.name .. " (" .. project.status .. ")",
                icon = "folder",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = "/pipelines/projects/" .. project.id }),
            })
        end
    end
    local questions = repo.open_questions()
    if #questions > 0 then
        table.insert(children, ui.text{ text = "Questions", size = "xs", weight = "semibold", tone = "muted" })
        for _, question in ipairs(questions) do
            local ticket = repo.get_ticket(question.ticket_id)
            table.insert(children, ui.button{
                label = (question.blocking == 1 and "Blocking: " or "") .. (ticket and ticket.title or question.ticket_id),
                icon = question.kind == "agent" and "user-circle" or "question-mark-circle",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = "/pipelines/tickets/" .. question.ticket_id }),
            })
        end
    end
    table.insert(children, ui.text{ text = "Tickets", size = "xs", weight = "semibold", tone = "muted" })
    for _, ticket in ipairs(view.visible_tickets(repo)) do
        local notifications = view.ticket_notification_count(ticket.id, repo)
        table.insert(children, ui.button{
            label = notifications > 0 and (ticket.title .. "  " .. tostring(notifications)) or ticket.title,
            icon = notifications > 0 and "exclamation-circle" or "ticket",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = "/pipelines/tickets/" .. ticket.id }),
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
