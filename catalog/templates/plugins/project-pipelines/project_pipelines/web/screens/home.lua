-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/home.lua
-- @scope device
-- @version 1.1.0

local view = require("project_pipelines.web.ui")

local M = {}

local function project_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        },
        start = {
            ui.status_dot{ state = ui.bind("@/status_state"), label = ui.bind("@/status_label") },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/status_label"), tone = ui.bind("@/status_tone") },
        },
    }
end

local function ticket_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/tail_label"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/latest_run_badge"), tone = ui.bind("@/latest_run_tone") },
            ui.badge{ text = ui.bind("@/secondary_badge"), tone = ui.bind("@/secondary_badge_tone") },
        },
    }
end

local function running_run_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.badge{ text = "running", tone = "accent" },
            ui.text{ text = ui.bind("@/ticket_title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/pipeline_name"), size = "xs", tone = "muted" },
            ui.text{ text = ui.bind("@/current_step_name"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.bind_if("@/has_ticket", ui.button{
                label = "Ticket",
                icon = "ticket",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ui.bind("@/ticket_path") }),
            }),
            ui.bind_if("@/has_project", ui.button{
                label = "Project",
                icon = "folder",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ui.bind("@/project_path") }),
            }),
            ui.button{
                label = "Run",
                icon = "play-circle",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
            },
            ui.bind_if("@/has_current_agent", ui.button{
                label = "Agent",
                icon = "command-line",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.nav.open", { path = ui.bind("@/current_agent_path") }),
            }),
        },
    }
end

local function merge_ticket_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.badge{ text = ui.bind("@/merge_status_label"), tone = ui.bind("@/merge_status_tone") },
            ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/merge_detail_label"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/target_label"), tone = "muted" },
            ui.bind_if("@/has_latest_run", ui.button{
                id = ui.bind("@/latest_run_button_id"),
                label = "Run",
                icon = "play-circle",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ui.bind("@/latest_run_path") }),
            }),
            ui.bind_if("@/has_merge_pr_url", ui.button{
                id = ui.bind("@/merge_pr_button_id"),
                label = "Open PR",
                icon = "external-link",
                variant = "ghost",
                action = ui.action("botster.url.open", { url = ui.bind("@/merge_pr_url") }),
            }),
            ui.bind_if("@/has_merge_session", ui.button{
                id = ui.bind("@/merge_session_button_id"),
                label = "Merge agent",
                icon = "command-line",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.nav.open", { path = ui.bind("@/merge_session_path") }),
            }),
        },
    }
end

local function question_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.text{ text = ui.bind("@/ticket_title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/question"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/kind_label"), tone = "muted" },
            ui.badge{ text = ui.bind("@/blocking_label"), tone = ui.bind("@/blocking_tone") },
        },
    }
end

local function pipeline_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/edit_path") }),
        title = {
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
        },
        end_ = {
            ui.text{ text = ui.bind("@/step_count_label"), size = "xs", tone = "muted" },
        },
    }
end

function M.render(_view_state, ctx)
    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = "Pipeline Workbench",
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
        view.panel{ view.section("Questions To Answer", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.question",
                    where = { status = "open" },
                    item_template = question_template(),
                    empty_template = view.empty(
                        "No open questions",
                        "Questions that need answers will appear here.",
                        "question-mark-circle"
                    ),
                },
            } },
        }) },
        view.panel{ view.section("Running Pipelines", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.run",
                    where = { status = "active" },
                    item_template = running_run_template(),
                    empty_template = view.empty(
                        "No running pipelines",
                        "Active ticket pipeline runs will appear here.",
                        "play-circle"
                    ),
                },
            } },
        }) },
        view.panel{ view.section("PRs And Merge", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.ticket",
                    where = { status = "open", latest_run_status = "done" },
                    item_template = merge_ticket_template(),
                    empty_template = view.empty(
                        "No tickets ready to merge",
                        "Completed runs waiting on PR or merge work will appear here.",
                        "code-bracket"
                    ),
                },
            } },
        }) },
        view.panel{ view.section("Projects", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.project",
                    where = { status = "open" },
                    item_template = project_template(),
                    empty_template = view.empty(
                        "No open projects",
                        "Projects with active pipeline work will appear here.",
                        "folder"
                    ),
                },
            } },
        }) },
        view.panel{ view.section("Standalone Tickets", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.ticket",
                    where = { status = "open", standalone = true },
                    item_template = ticket_template(),
                    empty_template = view.empty(
                        "No standalone tickets",
                        "Open tickets that are not part of a project will appear here.",
                        "ticket"
                    ),
                },
            } },
        }) },
        view.panel{ view.section("Pipeline Definitions", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.pipeline",
                    item_template = pipeline_template(),
                    empty_template = view.empty(
                        "No pipelines yet",
                        "Visit Pipeline index to create one, or ask an agent via the Project Pipelines MCP tools.",
                        "queue-list"
                    ),
                },
            } },
        }) },
    } }
end

return M
