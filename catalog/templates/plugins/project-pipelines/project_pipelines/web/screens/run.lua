-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/run.lua
-- @scope device
-- @version 1.1.0

local view = require("project_pipelines.web.ui")

local M = {}

local function stale_run_notice(run_id, ctx)
    return ui.bind_list{
        source = "/project-pipelines.run",
        where = { id = run_id },
        item_template = ui.stack{ direction = "vertical", gap = "1", children = {} },
        empty_template = view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.empty(
                    "Run not found",
                    "No run entity exists for run_id " .. tostring(run_id) .. ". The link may be stale or from another hub.",
                    "alert-triangle"
                ),
                ui.button{
                    id = "run-" .. tostring(run_id) .. "-not-found-back",
                    label = "Back",
                    icon = "arrow-left",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/") }),
                },
            } },
        },
    }
end

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
    local run_id = params.run_id
    if not run_id then
        return view.panel{ ui.text{ text = "Run not found", tone = "danger" } }
    end
    local run_path = "/project-pipelines.run/" .. run_id
    local header_actions = {
        ui.bind_if(run_path .. "/has_ticket", ui.button{
            id = ui.bind(run_path .. "/ticket_button_id"),
            label = "Ticket",
            icon = "ticket",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = ui.bind(run_path .. "/ticket_path") }),
        }),
        ui.bind_if(run_path .. "/has_project", ui.button{
            id = ui.bind(run_path .. "/project_button_id"),
            label = "Project",
            icon = "folder",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = ui.bind(run_path .. "/project_path") }),
        }),
        ui.bind_if(run_path .. "/has_current_agent", ui.button{
            id = ui.bind(run_path .. "/current_agent_button_id"),
            label = "Current agent",
            icon = "command-line",
            variant = "solid",
            tone = "accent",
            action = ui.action("botster.nav.open", { path = ui.bind(run_path .. "/current_agent_path") }),
        }),
    }

    return ui.stack{ direction = "vertical", gap = "4", children = {
        stale_run_notice(run_id, ctx),
        ui.bind_if(run_path .. "/id", ui.stack{ direction = "vertical", gap = "4", children = {
            view.page_header{
                title = ui.bind(run_path .. "/ticket_title"),
                back_id = "run-" .. run_id .. "-back",
                back_path = ctx.path("/"),
                meta = { view.badge(ui.bind(run_path .. "/status")) },
                actions = header_actions,
                description = ui.bind(run_path .. "/detail_label"),
            },
            view.section("Steps", step_nodes(run_id)),
            view.section("Reviews", review_nodes(run_id)),
            view.section("Findings", finding_nodes(run_id)),
            view.section("Artifacts", artifact_nodes(run_id)),
            view.section("Recent Events", event_nodes(run_id)),
        } }),
    } }
end

return M
