-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/run.lua
-- @scope device
-- @version 1.1.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")
local util = require("project_pipelines.util")

local M = {}

local function render_step(step, ctx, ticket_id)
    local detail = step.kind
    if step.kind == "agent" and step.agent_name then
        detail = detail .. " - selected agent: " .. step.agent_name
    elseif step.kind == "command" and step.command then
        detail = detail .. " - " .. step.command
    end

    local children = {
            view.row{
                view.badge("#" .. tostring(step.sequence or "?"), "muted"),
                ui.text{ text = step.name, size = "sm", weight = "semibold" },
                view.badge(step.status or step.kind),
            },
            ui.text{ text = detail, size = "xs", tone = "muted" },
            ui.text{ text = step.prompt or "", size = "xs", tone = "muted" },
    }
    if step.agent_session_uuid and step.agent_session_uuid ~= "" then
        table.insert(children, ui.button{
            id = "run-" .. step.run_id .. "-terminal-" .. step.id,
            label = "Open terminal",
            icon = "command-line",
            variant = "ghost",
            action = ui.action("botster.nav.open", {
                path = ctx.path("/tickets/" .. ticket_id .. "/sessions/" .. step.agent_session_uuid),
            }),
        })
    end
    return view.panel{ ui.stack{ direction = "vertical", gap = "1", children = children } }
end

local function review_nodes(run_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.review",
            where = { run_id = run_id },
            item_template = view.panel{
                ui.stack{ direction = "vertical", gap = "1", children = {
                    view.row{
                        view.badge(ui.bind("@/verdict")),
                        ui.text{ text = ui.bind("@/summary"), size = "sm", weight = "medium" },
                    },
                    ui.text{ text = ui.bind("@/reviewer_session_uuid"), size = "xs", tone = "muted" },
                } },
            },
            empty_template = ui.text{ text = "No reviews submitted.", size = "sm", tone = "muted" },
        },
    }
end

local function finding_nodes(run_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.finding",
            where = { run_id = run_id },
            item_template = view.panel{
                ui.stack{ direction = "vertical", gap = "1", children = {
                    view.row{
                        view.badge(ui.bind("@/severity")),
                        ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
                        view.badge(ui.bind("@/status")),
                    },
                    ui.text{ text = ui.bind("@/location_label"), size = "xs", tone = "muted" },
                    ui.text{ text = ui.bind("@/details"), size = "xs", tone = "muted" },
                } },
            },
            empty_template = ui.text{ text = "No findings recorded.", size = "sm", tone = "muted" },
        },
    }
end

local function artifact_nodes(run_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.artifact",
            where = { run_id = run_id },
            item_template = view.panel{
                ui.stack{ direction = "vertical", gap = "1", children = {
                    view.row{
                        view.badge(ui.bind("@/kind"), "muted"),
                        ui.text{
                            text = ui.bind("@/display_summary"),
                            size = "sm",
                            weight = "medium",
                        },
                    },
                    ui.text{ text = ui.bind("@/uri"), size = "xs", tone = "muted" },
                } },
            },
            empty_template = ui.text{ text = "No artifacts attached.", size = "sm", tone = "muted" },
        },
    }
end

local function event_nodes(run_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.event",
            where = { run_id = run_id },
            item_template = ui.text{ text = ui.bind("@/kind"), size = "xs", tone = "muted" },
            empty_template = ui.text{ text = "No events yet.", size = "sm", tone = "muted" },
        },
    }
end

function M.render(view_state, ctx)
    local params = view_state and view_state.params or {}
    local overview = repo.run_detail_overview(params.run_id)
    local run = overview and overview.run or nil
    if not run then
        return view.panel{ ui.text{ text = "Run not found", tone = "danger" } }
    end

    local ticket = overview.ticket
    local pipeline = overview.pipeline
    local project = ticket and not util.is_blank(ticket.project_id) and repo.get_project(ticket.project_id) or nil
    local step_nodes = {}
    for _, step in ipairs(overview.steps) do
        table.insert(step_nodes, render_step(step, ctx, run.ticket_id))
    end

    local header_actions = {}
    if ticket then
        header_actions[#header_actions + 1] = ui.button{
            id = "run-" .. run.id .. "-ticket",
            label = "Ticket",
            icon = "ticket",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = ctx.path("/tickets/" .. ticket.id) }),
        }
    end
    if project then
        header_actions[#header_actions + 1] = ui.button{
            id = "run-" .. run.id .. "-project",
            label = "Project",
            icon = "folder",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. project.id) }),
        }
    end
    local current = run.current_run_step_id and repo.get_run_step_visit(run.current_run_step_id) or nil
    if current and not util.is_blank(current.agent_session_uuid) and view.session_info(current.agent_session_uuid) then
        header_actions[#header_actions + 1] = ui.button{
            id = "run-" .. run.id .. "-current-agent",
            label = "Current agent",
            icon = "command-line",
            variant = "solid",
            tone = "accent",
            action = ui.action("botster.nav.open", {
                path = ctx.path("/tickets/" .. run.ticket_id .. "/sessions/" .. current.agent_session_uuid),
            }),
        }
    end

    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = ticket and ticket.title or run.id,
            back_id = "run-" .. run.id .. "-back",
            back_path = ctx.path("/"),
            meta = { view.badge(run.status) },
            actions = header_actions,
            description = (pipeline and pipeline.name or run.pipeline_id) .. " - current step: " .. (run.current_step_id or "none"),
        },
        view.section("Steps", step_nodes),
        view.section("Reviews", review_nodes(run.id)),
        view.section("Findings", finding_nodes(run.id)),
        view.section("Artifacts", artifact_nodes(run.id)),
        view.section("Recent Events", event_nodes(run.id)),
    } }
end

return M
