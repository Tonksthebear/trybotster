-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/home.lua
-- @scope device
-- @version 1.0.0

local view = require("project_pipelines.web.ui")
local repo = require("project_pipelines.repo")

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

local function notified_agent_rows(ctx)
    local ok, Agent = pcall(require, "lib.agent")
    if not ok or not Agent or type(Agent.list) ~= "function" then
        return {}
    end

    local rows = {}
    local notified_uuids = {}
    for _, session in ipairs(Agent.list() or {}) do
        local ok_info, info = pcall(session.info, session)
        if ok_info and info and info.notification == true then
            local uuid = info.session_uuid or info.id
            if uuid and uuid ~= "" then
                notified_uuids[#notified_uuids + 1] = uuid
                rows[#rows + 1] = {
                    uuid = uuid,
                    label = info.label or info.display_name or info.title or info.agent_name or uuid,
                }
            end
        end
    end

    local tickets_by_uuid = repo.ticket_session_links_for_uuids(notified_uuids)
    for _, row in ipairs(rows) do
        row.ticket = (tickets_by_uuid[row.uuid] or {})[1]
    end

    table.sort(rows, function(a, b)
        return tostring(a.label) < tostring(b.label)
    end)

    local children = {}
    for _, row in ipairs(limit(rows, 8)) do
        local ticket = row.ticket
        children[#children + 1] = ui.list_item{
            id = "pipelines-home-agent-" .. row.uuid,
            action = ticket and ui.action("botster.nav.open", {
                path = ctx.path("/tickets/" .. ticket.id .. "/sessions/" .. row.uuid),
            }) or nil,
            title = {
                ui.text{ text = row.label, size = "sm", weight = "semibold" },
            },
            subtitle = {
                ui.text{ text = ticket and ticket.title or row.uuid, size = "xs", tone = "muted" },
            },
            end_ = {
                view.badge("needs attention", "danger"),
            },
        }
    end
    return children
end

local function run_template()
    return ui.list_item{
        id = ui.bind("@/id"),
        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
        title = {
            ui.text{ text = ui.bind("@/ticket_title"), size = "sm", weight = "semibold" },
        },
        subtitle = {
            ui.text{ text = ui.bind("@/pipeline_name"), size = "xs", tone = "muted" },
        },
        end_ = {
            ui.badge{ text = ui.bind("@/status"), tone = ui.bind("@/status_tone") },
            ui.text{ text = ui.bind("@/current_step_name"), size = "xs", tone = "muted" },
        },
    }
end

local function list_or_empty(items, renderer, empty)
    if #items == 0 then
        return { empty }
    end
    local children = {}
    for _, item in ipairs(items) do
        children[#children + 1] = renderer(item)
    end
    return { ui.list{ children = children } }
end

function M.render(_view_state, ctx)
    local notified_agents = notified_agent_rows(ctx)

    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = "Project Pipelines",
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
        view.panel{ view.section("Open Questions", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.question",
                    where = { status = "open" },
                    item_template = question_template(),
                },
            } },
        }) },
        view.panel{ view.section("Agents Needing Attention", #notified_agents > 0 and { ui.list{ children = notified_agents } } or {
            view.empty("No agent notifications", "Agents that need attention will appear here.", "bell"),
        }) },
        view.panel{ view.section("Open Runs", {
            ui.list{ children = {
                ui.bind_list{
                    source = "/project-pipelines.run",
                    where = { status = "active" },
                    item_template = run_template(),
                },
                ui.bind_list{
                    source = "/project-pipelines.run",
                    where = { status = "blocked" },
                    item_template = run_template(),
                },
            } },
        }) },
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
