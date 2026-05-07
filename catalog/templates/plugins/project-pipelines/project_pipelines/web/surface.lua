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
    return repo.has_open_questions()
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

local function sidebar_project(project)
    return ui.list_item{
        id = project and ("pipelines-sidebar-project-" .. project.id) or ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = project and ("/pipelines/projects/" .. project.id) or ui.bind("@/path") }),
        start = {
            project and view.status_mark(project.status) or ui.status_dot{ state = ui.bind("@/status_state"), label = ui.bind("@/status_label") },
        },
        title = {
            ui.text{ text = project and project.name or ui.bind("@/name"), size = "sm", weight = "semibold" },
        },
    }
end

local function sidebar_question_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        start = {
            ui.status_dot{ state = "danger", label = "?" },
        },
        title = {
            ui.text{ text = ui.bind("@/ticket_title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/question"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/blocking_label"), tone = ui.bind("@/blocking_tone") },
        },
    }
end

local function sidebar_ticket_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        start = {
            ui.status_dot{ state = ui.bind("@/status_state"), label = ui.bind("@/status_label") },
        },
        title = {
            ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/tail_label"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/latest_run_badge"), tone = ui.bind("@/latest_run_tone") },
        },
    }
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
    table.insert(children, ui.text{ text = "Projects", size = "xs", weight = "semibold", tone = "muted" })
    table.insert(children, ui.bind_list{
        source = "/project-pipelines.project",
        where = { status = "open" },
        item_template = sidebar_project(),
    })
    table.insert(children, ui.text{ text = "Questions", size = "xs", weight = "semibold", tone = "muted" })
    table.insert(children, ui.bind_list{
        source = "/project-pipelines.question",
        where = { status = "open" },
        item_template = sidebar_question_template(),
    })
    table.insert(children, ui.text{ text = "Tickets", size = "xs", weight = "semibold", tone = "muted" })
    table.insert(children, ui.bind_list{
        source = "/project-pipelines.ticket",
        where = { status = "open", standalone = true },
        item_template = sidebar_ticket_template(),
    })

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
