-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/project.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

local M = {}
local PERF = os.getenv("BOTSTER_LUA_PERF") == "1"

local function elapsed_ms(started)
    return math.floor(((os.clock() - started) * 1000) + 0.5)
end

local function log_perf(message)
    if PERF and log and log.info then
        log.info("[PERF][project_pipelines.project] " .. message)
    end
end

local function project_target_nodes(project_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.project_target",
            where = { project_id = project_id },
            item_template = ui.badge{ text = ui.bind("@/target_label"), tone = "accent" },
        },
    }
end

local function dependency_tree_nodes(project_id)
    return {
        ui.bind_list{
            source = "/project-pipelines.ticket",
            where = { project_id = project_id },
            item_template = view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge(ui.bind("@/project_stage_label"), "muted"),
                    ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
                    view.badge(ui.bind("@/latest_run_badge"), ui.bind("@/latest_run_tone")),
                },
                ui.text{ text = ui.bind("@/dependency_summary"), size = "xs", tone = "muted" },
                ui.text{ text = ui.bind("@/tail_label"), size = "xs", tone = "muted" },
                view.row{
                    view.badge(ui.bind("@/target_label"), "accent"),
                    ui.button{
                        id = ui.bind("@/id"),
                        label = "Open ticket",
                        icon = "arrow-right",
                        variant = "ghost",
                        action = ui.action("botster.nav.open", { path = ui.bind("@/path") }),
                    },
                },
            } },
        },
        },
    }
end

function M.render(view_state, ctx)
    local render_started = PERF and os.clock() or nil
    local params = view_state and view_state.params or {}
    local started = PERF and os.clock() or nil
    local project = repo.get_project(params.project_id)
    if started then
        log_perf(string.format(
            "phase=get_project project_id=%s elapsed_ms=%d",
            tostring(params.project_id),
            elapsed_ms(started)))
    end
    if not project then
        return view.panel{ ui.text{ text = "Project not found", tone = "danger" } }
    end
    started = PERF and os.clock() or nil
    local target_nodes = project_target_nodes(project.id)
    if started then
        log_perf(string.format(
            "phase=project_targets project_id=%s nodes=%d elapsed_ms=%d",
            tostring(project.id),
            #target_nodes,
            elapsed_ms(started)))
    end

    started = PERF and os.clock() or nil
    local dependency_nodes = dependency_tree_nodes(project.id)
    if started then
        log_perf(string.format(
            "phase=dependency_tree project_id=%s nodes=%d elapsed_ms=%d",
            tostring(project.id),
            #dependency_nodes,
            elapsed_ms(started)))
    end

    local node = ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = project.name,
            back_id = "project-" .. project.id .. "-back",
            back_path = ctx.path("/"),
            meta = { view.status_mark(project.status) },
            actions = {
                ui.button{
                    id = "project-" .. project.id .. "-new-ticket",
                    label = "New ticket",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", { path = ctx.path("/projects/" .. project.id .. "/new-ticket") }),
                },
            },
            description = project.description or "",
        },
        view.section("Project Targets", target_nodes),
        view.section("Tickets", dependency_nodes),
    } }
    if render_started then
        log_perf(string.format(
            "phase=render_total project_id=%s elapsed_ms=%d",
            tostring(project.id),
            elapsed_ms(render_started)))
    end
    return node
end

return M
