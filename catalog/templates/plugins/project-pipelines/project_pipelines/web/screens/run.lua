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

local function step_nodes(run_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.run_step",
            where = { run_id = run_id },
            item_template = view.panel{
                ui.stack{ direction = "vertical", gap = "1", children = {
                    view.row{
                        view.badge(ui.bind("@/sequence_label"), "muted"),
                        ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
                        view.badge(ui.bind("@/status_label")),
                    },
                    ui.text{ text = ui.bind("@/detail"), size = "xs", tone = "muted" },
                    ui.text{ text = ui.bind("@/prompt_text"), size = "xs", tone = "muted" },
                    ui.bind_if("@/has_terminal", ui.button{
                        id = ui.bind("@/terminal_button_id"),
                        label = "Open terminal",
                        icon = "command-line",
                        variant = "ghost",
                        action = ui.action("botster.nav.open", {
                            path = ui.bind("@/terminal_path"),
                        }),
                    }),
                } },
            },
            empty_template = ui.text{ text = "No steps recorded.", size = "sm", tone = "muted" },
        },
    }
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
    local run = repo.get_run(params.run_id)
    if not run then
        return view.panel{ ui.text{ text = "Run not found", tone = "danger" } }
    end

    local run_path = "/project-pipelines.run/" .. run.id
    local ticket = repo.get_ticket(run.ticket_id)
    local project = ticket and not util.is_blank(ticket.project_id) and repo.get_project(ticket.project_id) or nil

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
            title = ui.bind(run_path .. "/ticket_title"),
            back_id = "run-" .. run.id .. "-back",
            back_path = ctx.path("/"),
            meta = { view.badge(ui.bind(run_path .. "/status")) },
            actions = header_actions,
            description = ui.bind(run_path .. "/detail_label"),
        },
        view.section("Steps", step_nodes(run.id)),
        view.section("Reviews", review_nodes(run.id)),
        view.section("Findings", finding_nodes(run.id)),
        view.section("Artifacts", artifact_nodes(run.id)),
        view.section("Recent Events", event_nodes(run.id)),
    } }
end

return M
