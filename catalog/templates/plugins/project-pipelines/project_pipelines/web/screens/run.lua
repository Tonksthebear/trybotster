-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/run.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")

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

local function review_nodes(run_id, overview)
    local nodes = {}
    local rows = overview and overview.reviews or repo.run_reviews(run_id)
    if #rows == 0 then
        return { ui.text{ text = "No reviews submitted.", size = "sm", tone = "muted" } }
    end

    for _, review in ipairs(rows) do
        table.insert(nodes, view.panel{
            ui.stack{ direction = "vertical", gap = "1", children = {
                view.row{
                    view.badge(review.verdict),
                    ui.text{ text = review.summary or "", size = "sm", weight = "medium" },
                },
                ui.text{ text = review.reviewer_session_uuid or "", size = "xs", tone = "muted" },
            } },
        })
    end
    return nodes
end

local function finding_nodes(run_id, overview)
    local nodes = {}
    local rows = overview and overview.findings or repo.run_findings(run_id)
    if #rows == 0 then
        return { ui.text{ text = "No findings recorded.", size = "sm", tone = "muted" } }
    end

    for _, finding in ipairs(rows) do
        table.insert(nodes, view.panel{
            ui.stack{ direction = "vertical", gap = "1", children = {
                view.row{
                    view.badge(finding.severity),
                    ui.text{ text = finding.title, size = "sm", weight = "semibold" },
                    view.badge(finding.status),
                },
                ui.text{
                    text = (finding.file or "") .. (finding.line and (":" .. tostring(finding.line)) or ""),
                    size = "xs",
                    tone = "muted",
                },
                ui.text{ text = finding.details or "", size = "xs", tone = "muted" },
            } },
        })
    end
    return nodes
end

local function artifact_nodes(run_id, overview)
    local nodes = {}
    local rows = overview and overview.artifacts or repo.run_artifacts(run_id)
    if #rows == 0 then
        return { ui.text{ text = "No artifacts attached.", size = "sm", tone = "muted" } }
    end

    for _, artifact in ipairs(rows) do
        table.insert(nodes, view.panel{
            ui.stack{ direction = "vertical", gap = "1", children = {
                view.row{
                    view.badge(artifact.kind, "muted"),
                    ui.text{
                        text = artifact.summary or artifact.uri or artifact.id,
                        size = "sm",
                        weight = "medium",
                    },
                },
                ui.text{ text = artifact.uri or "", size = "xs", tone = "muted" },
            } },
        })
    end
    return nodes
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
    local step_nodes = {}
    for _, step in ipairs(overview.steps) do
        table.insert(step_nodes, render_step(step, ctx, run.ticket_id))
    end

    local event_nodes = {}
    for _, event in ipairs(overview.events) do
        table.insert(event_nodes, ui.text{ text = event.kind, size = "xs", tone = "muted" })
    end
    if #event_nodes == 0 then
        table.insert(event_nodes, ui.text{ text = "No events yet.", size = "sm", tone = "muted" })
    end

    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = ticket and ticket.title or run.id,
            back_id = "run-" .. run.id .. "-back",
            back_path = ctx.path("/"),
            meta = { view.badge(run.status) },
            description = (pipeline and pipeline.name or run.pipeline_id) .. " - current step: " .. (run.current_step_id or "none"),
        },
        view.section("Steps", step_nodes),
        view.section("Reviews", review_nodes(run.id, overview)),
        view.section("Findings", finding_nodes(run.id, overview)),
        view.section("Artifacts", artifact_nodes(run.id, overview)),
        view.section("Recent Events", event_nodes),
    } }
end

return M
