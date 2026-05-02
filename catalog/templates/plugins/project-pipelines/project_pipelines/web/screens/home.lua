-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/home.lua
-- @scope device
-- @version 1.0.0

local view = require("project_pipelines.web.ui")

local M = {}

local function ticket_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/latest_run_badge"), tone = ui.bind("@/latest_run_tone") },
            ui.badge{ text = ui.bind("@/secondary_badge"), tone = ui.bind("@/secondary_badge_tone") },
        },
        detail = {
            ui.text{ text = ui.bind("@/run_count_label"), size = "xs", tone = "muted" },
            ui.text{ text = ui.bind("@/tail_label"), size = "xs", tone = "muted" },
        },
    }
end

local function pipeline_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", {
            path = ui.bind("@/edit_path"),
        }),
        title = {
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.text{ text = ui.bind("@/step_count_label"), size = "xs", tone = "muted" },
        },
    }
end

local function project_template()
    return ui.tree_item{
        id = ui.bind("@/id"),
        expanded = true,
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/status"), tone = ui.bind("@/status_tone") },
        },
    }
end

function M.render(_view_state, ctx)
    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = "Project Pipelines",
            description = "Pipeline state, gate prompts, reviews, findings, artifacts, and plugin-owned sessions are persisted here.",
            actions = {
                ui.button{
                    id = "pipelines-home-new-ticket",
                    label = "New ticket",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/new-ticket") }),
                },
                ui.button{
                    id = "pipelines-home-new-project",
                    label = "New project",
                    icon = "folder-plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/new-project") }),
                },
                ui.button{
                    id = "pipelines-home-index",
                    label = "Pipeline index",
                    icon = "queue-list",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/pipelines") }),
                },
            },
        },
        view.panel{ view.section("Projects", {
            ui.tree{ children = {
                ui.bind_list{ source = "/project-pipelines.project", item_template = project_template() },
            } },
        }) },
        view.panel{ view.section("Tickets", {
            ui.list{ children = {
                ui.bind_list{ source = "/project-pipelines.ticket", item_template = ticket_template() },
            } },
        }) },
        view.panel{ view.section("Recent Runs", {
            ui.table{
                columns = {
                    { key = "pipeline_name", label = "Pipeline" },
                    { key = "status", label = "Status" },
                    { key = "ticket_title", label = "Ticket" },
                },
                rows = ui.bind("/project-pipelines.run"),
            },
        }) },
        view.section("Pipeline Definitions", {
            ui.list{ children = {
                ui.bind_list{ source = "/project-pipelines.pipeline", item_template = pipeline_template() },
            } },
        }),
    } }
end

return M
