-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/run.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local M = {}

local function step_template()
    local children = {
            view.row{
                view.badge(ui.bind("@/sequence"), "muted"),
                ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
                view.badge(ui.bind("@/status")),
            },
            ui.text{ text = ui.bind("@/kind"), size = "xs", tone = "muted" },
            ui.text{ text = ui.bind("@/prompt"), size = "xs", tone = "muted" },
    }
    return view.panel{ ui.stack{ direction = "vertical", gap = "1", children = children } }
end

local function review_template()
    return view.panel{
            ui.stack{ direction = "vertical", gap = "1", children = {
                view.row{
                    view.badge(ui.bind("@/verdict")),
                    ui.text{ text = ui.bind("@/summary"), size = "sm", weight = "medium" },
                },
                ui.text{ text = ui.bind("@/reviewer_session_uuid"), size = "xs", tone = "muted" },
            } },
        }
end

local function finding_template()
    return view.panel{
            ui.stack{ direction = "vertical", gap = "1", children = {
                view.row{
                    view.badge(ui.bind("@/severity")),
                    ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
                    view.badge(ui.bind("@/status")),
                },
                ui.text{ text = ui.bind("@/file"), size = "xs", tone = "muted" },
                ui.text{ text = ui.bind("@/details"), size = "xs", tone = "muted" },
            } },
        }
end

local function artifact_template()
    return view.panel{
            ui.stack{ direction = "vertical", gap = "1", children = {
                view.row{
                    view.badge(ui.bind("@/kind"), "muted"),
                    ui.text{
                        text = ui.bind("@/summary"),
                        size = "sm",
                        weight = "medium",
                    },
                },
                ui.text{ text = ui.bind("@/uri"), size = "xs", tone = "muted" },
            } },
        }
end

local function event_template()
    return ui.text{ text = ui.bind("@/kind"), size = "xs", tone = "muted" }
end

function M.render(view_state, ctx)
    local params = view_state and view_state.params or {}
    local run = repo.get_run(params.run_id)
    if not run then
        return view.panel{ ui.text{ text = "Run not found", tone = "danger" } }
    end

    local ticket = repo.get_ticket(run.ticket_id)
    local pipeline = repo.get_pipeline(run.pipeline_id)
    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
            view.row{
                ui.button{
                    label = "Back",
                    icon = "arrow-left",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/") }),
                },
                ui.text{ text = ticket and ticket.title or run.id, size = "lg", weight = "semibold" },
                view.badge(run.status),
            },
            ui.text{
                text = (pipeline and pipeline.name or run.pipeline_id) .. " - current step: " .. (run.current_step_id or "none"),
                size = "sm",
                tone = "muted",
            },
        } } },
        view.section("Steps", {
            ui.bind_list{ source = "/project-pipelines.run_step", where = { run_id = run.id }, item_template = step_template() },
        }),
        view.section("Reviews", {
            ui.bind_list{ source = "/project-pipelines.review", where = { run_id = run.id }, item_template = review_template() },
        }),
        view.section("Findings", {
            ui.bind_list{ source = "/project-pipelines.finding", where = { run_id = run.id }, item_template = finding_template() },
        }),
        view.section("Artifacts", {
            ui.bind_list{ source = "/project-pipelines.artifact", where = { run_id = run.id }, item_template = artifact_template() },
        }),
        view.section("Recent Events", {
            ui.bind_list{ source = "/project-pipelines.event", where = { run_id = run.id }, item_template = event_template() },
        }),
    } }
end

return M
