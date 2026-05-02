-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/home.lua
-- @scope device
-- @version 1.0.0

local view = require("project_pipelines.web.ui")

local M = {}

local function ticket_template()
    return view.panel{
        ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
                ui.badge{ text = ui.bind("@/latest_run_badge"), tone = ui.bind("@/latest_run_tone") },
                ui.badge{ text = ui.bind("@/secondary_badge"), tone = ui.bind("@/secondary_badge_tone") },
            },
            ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
            view.row{
                ui.text{ text = ui.bind("@/run_count_label"), size = "xs", tone = "muted" },
                ui.text{ text = ui.bind("@/tail_label"), size = "xs", tone = "muted" },
                ui.button{
                    label = "Open ticket",
                    icon = "arrow-right",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
                },
            },
        } },
    }
end

local function pipeline_template()
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
        view.row{
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
            ui.button{
                label = "Edit",
                icon = "pencil-square",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.nav.open", {
                    path = ui.bind("@/edit_path"),
                }),
            },
        },
        ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        ui.text{ text = ui.bind("@/step_count_label"), size = "xs", tone = "muted" },
    } } }
end

local function project_template()
    return view.panel{
        ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
                ui.badge{ text = ui.bind("@/status"), tone = ui.bind("@/status_tone") },
            },
            ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
            ui.button{
                label = "Open project",
                icon = "folder-open",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
            },
        } },
    }
end

local function run_template()
    return ui.button{
        label = ui.bind("@/label"),
        icon = "queue-list",
        variant = "ghost",
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
    }
end

function M.render(_view_state, ctx)
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
        view.panel{ view.section("Projects", {
            ui.bind_list{ source = "/project-pipelines.project", item_template = project_template() },
        }) },
        view.panel{ view.section("Tickets", {
            ui.bind_list{ source = "/project-pipelines.ticket", item_template = ticket_template() },
        }) },
        view.panel{ view.section("Recent Runs", {
            ui.bind_list{ source = "/project-pipelines.run", item_template = run_template() },
        }) },
        view.section("Pipeline Definitions", {
            ui.bind_list{ source = "/project-pipelines.pipeline", item_template = pipeline_template() },
        }),
    } }
end

return M
