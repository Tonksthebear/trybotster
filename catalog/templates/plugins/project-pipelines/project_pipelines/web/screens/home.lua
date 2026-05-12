-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/home.lua
-- @scope device
-- @version 1.0.0

local view = require("project_pipelines.web.ui")
local repo = require("project_pipelines.repo")
local util = require("project_pipelines.util")

local M = {}

local function limit(items, count)
    local out = {}
    for index, item in ipairs(items or {}) do
        if index > count then
            break
        end
        out[#out + 1] = item
    end
    return out
end

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

local function session_label(session_uuid, fallback)
    local info = view.session_info(session_uuid)
    if not info then
        return fallback or session_uuid
    end
    return info.label or info.display_name or info.title or info.agent_name or fallback or session_uuid
end

local function current_run_step(run)
    if not run or util.is_blank(run.current_run_step_id) then
        return nil
    end
    return repo.get_run_step_visit(run.current_run_step_id)
end

local function ticket_button(ticket, ctx)
    return ui.button{
        id = "home-ticket-" .. ticket.id,
        label = "Ticket",
        icon = "ticket",
        variant = "ghost",
        action = ui.action("botster.nav.open", { path = ctx.path("/tickets/" .. ticket.id) }),
    }
end

local function project_button(project_id, ctx)
    if util.is_blank(project_id) then
        return nil
    end
    return ui.button{
        id = "home-project-" .. project_id,
        label = "Project",
        icon = "folder",
        variant = "ghost",
        action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. project_id) }),
    }
end

local function running_pipeline_rows(ctx)
    local children = {}
    for _, run in ipairs(repo.list_runs(40)) do
        if run.status == "active" then
            local ticket = repo.get_ticket(run.ticket_id)
            local pipeline = repo.get_pipeline(run.pipeline_id)
            local step = run.current_step_id and repo.get_step(run.current_step_id) or nil
            local run_step = current_run_step(run)
            local actions = {}
            if ticket then
                actions[#actions + 1] = ticket_button(ticket, ctx)
                local project_action = project_button(ticket.project_id, ctx)
                if project_action then
                    actions[#actions + 1] = project_action
                end
            end
            actions[#actions + 1] = ui.button{
                id = "home-run-" .. run.id,
                label = "Run",
                icon = "queue-list",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.nav.open", { path = ctx.path("/runs/" .. run.id) }),
            }
            if run_step and not util.is_blank(run_step.agent_session_uuid) and view.session_info(run_step.agent_session_uuid) then
                actions[#actions + 1] = ui.button{
                    id = "home-run-agent-" .. run_step.agent_session_uuid,
                    label = "Agent",
                    icon = "command-line",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", {
                        path = ctx.path("/tickets/" .. run.ticket_id .. "/sessions/" .. run_step.agent_session_uuid),
                    }),
                }
            end
            children[#children + 1] = view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        view.badge("running", "accent"),
                        ui.text{ text = ticket and ticket.title or run.ticket_id, size = "sm", weight = "semibold" },
                    },
                    ui.text{
                        text = (pipeline and pipeline.name or run.pipeline_id) .. " · " .. (step and step.name or "No current step"),
                        size = "xs",
                        tone = "muted",
                    },
                    view.action_row(actions),
                } },
            }
        end
    end
    if #children == 0 then
        return { view.empty("No running pipelines", "Active ticket runs will appear here.", "queue-list") }
    end
    return limit(children, 8)
end

local function merge_work_rows(ctx)
    local children = {}
    for _, ticket in ipairs(repo.visible_tickets()) do
        local run = repo.latest_ticket_run(ticket.id)
        if run and run.status == "done" and ticket.status ~= "closed" then
            local merge_events = repo.ticket_events(ticket.id, "ticket.merge_requested", 1)
            local artifact = repo.latest_merge_pr_artifact(run.id)
            local payload = artifact and util.decode(artifact.payload, {}) or {}
            local pr_url = artifact and (artifact.uri or payload.pr_url) or nil
            local merge_payload = merge_events[1] and util.decode(merge_events[1].payload, {}) or {}
            local session_uuid = merge_payload.session_uuid
            local label = pr_url and "PR needs review" or (#merge_events > 0 and "merge agent running" or "ready for merge")
            local actions = { ticket_button(ticket, ctx) }
            local project_action = project_button(ticket.project_id, ctx)
            if project_action then
                actions[#actions + 1] = project_action
            end
            actions[#actions + 1] = ui.button{
                id = "home-merge-run-" .. run.id,
                label = "Run",
                icon = "queue-list",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ctx.path("/runs/" .. run.id) }),
            }
            if pr_url and pr_url ~= "" then
                actions[#actions + 1] = ui.button{
                    id = "home-pr-" .. run.id,
                    label = "Open PR",
                    icon = "external-link",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.url.open", { url = pr_url }),
                }
            elseif not util.is_blank(session_uuid) and view.session_info(session_uuid) then
                actions[#actions + 1] = ui.button{
                    id = "home-merge-agent-" .. session_uuid,
                    label = "Merge agent",
                    icon = "command-line",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", {
                        path = ctx.path("/tickets/" .. ticket.id .. "/sessions/" .. session_uuid),
                    }),
                }
            end
            children[#children + 1] = view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        view.badge(label, pr_url and "danger" or "accent"),
                        ui.text{ text = ticket.title, size = "sm", weight = "semibold" },
                    },
                    ui.text{
                        text = pr_url or session_label(session_uuid, "No PR recorded yet."),
                        size = "xs",
                        tone = "muted",
                    },
                    view.action_row(actions),
                } },
            }
        end
    end
    if #children == 0 then
        return { view.empty("No PRs waiting", "Open PRs and merge agents will appear here.", "git-pull-request") }
    end
    return limit(children, 8)
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
                },
            } },
        }) },
        view.panel{ view.section("Running Pipelines", running_pipeline_rows(ctx)) },
        view.panel{ view.section("PRs And Merge", merge_work_rows(ctx)) },
        view.panel{ view.section("Projects", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.project",
                    where = { status = "open" },
                    item_template = project_template(),
                },
            } },
        }) },
        view.panel{ view.section("Standalone Tickets", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.ticket",
                    where = { status = "open", standalone = true },
                    item_template = ticket_template(),
                },
            } },
        }) },
        view.panel{ view.section("Pipeline Definitions", {
            ui.list{ children = {
                ui.bind_list{ source = "/project-pipelines.pipeline", item_template = pipeline_template() },
            } },
        }) },
    } }
end

return M
