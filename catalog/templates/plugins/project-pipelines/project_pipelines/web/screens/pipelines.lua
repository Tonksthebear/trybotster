-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/pipelines.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local view = require("project_pipelines.web.ui")
local actions = require("project_pipelines.web.actions")

local M = {}

local function pipeline_step_summary_template()
    local bits = {
        view.badge(ui.bind("@/position"), "muted"),
        ui.text{ text = ui.bind("@/name"), size = "xs", weight = "medium" },
        view.badge(ui.bind("@/kind"), "muted"),
    }
    return view.row(bits)
end

local function pipeline_card_template(ctx)
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
        view.row{
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
            ui.button{
                label = "Edit",
                icon = "pencil-square",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ui.bind("@/edit_path") }),
            },
        },
        ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        ui.text{ text = ui.bind("@/step_count_label"), size = "xs", tone = "muted" },
    } } }
end

function M.index(_view_state, ctx)
    local children = {
        view.row{
            ui.button{
                label = "Back",
                icon = "arrow-left",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ctx.path("/") }),
            },
            ui.text{ text = "Pipeline Definitions", size = "lg", weight = "semibold" },
        },
    }

    table.insert(children, ui.bind_list{
        source = "/project-pipelines.pipeline",
        item_template = pipeline_card_template(ctx),
    })

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

local function feedback_error(state, id, field)
    local errors = state and state.field_errors or {}
    return errors[tostring(id or "") .. ":" .. tostring(field or "")]
end

local function step_options(steps, selected)
    local options = {
        { value = "", label = "Linear order" },
    }
    local found = selected == nil or selected == ""
    for _, step in ipairs(steps or {}) do
        table.insert(options, {
            value = step.id,
            label = "#" .. tostring(step.position) .. " - " .. step.name,
        })
        if step.id == selected then
            found = true
        end
    end
    if not found then
        table.insert(options, { value = selected, label = selected })
    end
    return options
end

local function edit_pipeline_fields(pipeline, state)
    local children = {
        ui.text_input{
            id = "pipeline-" .. pipeline.id .. "-name",
            label = "Name",
            placeholder = pipeline.name or "",
            on_change = view.field_action("project_pipelines.update_pipeline_field", {
                pipeline_id = pipeline.id,
                field = "name",
            }),
        },
    }
    local name_error = feedback_error(state, pipeline.id, "name")
    if name_error then
        table.insert(children, ui.text{ text = name_error, size = "xs", tone = "danger" })
    end
    table.insert(children, ui.textarea{
        id = "pipeline-" .. pipeline.id .. "-description",
        label = "Description",
        placeholder = pipeline.description or "",
        on_change = view.field_action("project_pipelines.update_pipeline_field", {
            pipeline_id = pipeline.id,
            field = "description",
        }),
    })
    local description_error = feedback_error(state, pipeline.id, "description")
    if description_error then
        table.insert(children, ui.text{ text = description_error, size = "xs", tone = "danger" })
    end
    return view.panel{
        ui.stack{ direction = "vertical", gap = "3", children = children },
    }
end

local function edit_gate_template(state)
    local children = {
        view.row{
            view.badge(ui.bind("@/kind"), "muted"),
            ui.text{ text = ui.bind("@/id"), size = "xs", tone = "muted" },
        },
        ui.textarea{
            id = ui.bind("@/id"),
            label = "Gate prompt",
            placeholder = ui.bind("@/prompt"),
            on_change = view.field_action("project_pipelines.update_gate_field", {
                gate_id = ui.bind("@/id"),
                field = "prompt",
            }),
        },
    }
    local _state = state
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = children } }
end

local function edit_step(step, steps, state)
    local children = {
        view.row{
            view.badge(step.position, "muted"),
            ui.text{ text = step.name, size = "sm", weight = "semibold" },
            view.badge(step.kind, "muted"),
        },
        ui.textarea{
            id = "step-" .. step.id .. "-prompt",
            label = "Agent prompt",
            placeholder = step.prompt or "",
            on_change = view.field_action("project_pipelines.update_step_field", {
                step_id = step.id,
                field = "prompt",
            }),
        },
    }

    if step.kind == "agent" then
        table.insert(children, ui.select{
            id = "step-" .. step.id .. "-agent",
            label = "Selected agent",
            placeholder = step.agent_name or "Select agent",
            options = view.agent_options(step.agent_name),
            on_change = view.field_action("project_pipelines.update_step_field", {
                step_id = step.id,
                field = "agent_name",
            }),
        })
    end

    if step.kind == "command" then
        table.insert(children, ui.text_input{
            id = "step-" .. step.id .. "-command",
            label = "Command",
            placeholder = step.command or "",
            on_change = view.field_action("project_pipelines.update_step_field", {
                step_id = step.id,
                field = "command",
            }),
        })
    end

    table.insert(children, ui.text{ text = "Transitions", size = "xs", weight = "semibold" })
    for _, transition in ipairs({
        { field = "next_step_id", label = "Default next step" },
        { field = "on_approved_step_id", label = "On review approved" },
        { field = "on_changes_requested_step_id", label = "On changes requested" },
        { field = "on_blocked_step_id", label = "On blocked" },
    }) do
        table.insert(children, ui.select{
            id = "step-" .. step.id .. "-" .. transition.field,
            label = transition.label,
            value = step[transition.field],
            placeholder = "Linear order",
            options = step_options(steps, step[transition.field]),
            on_change = view.field_action("project_pipelines.update_step_field", {
                step_id = step.id,
                field = transition.field,
            }),
        })
        local err = feedback_error(state, step.id, transition.field)
        if err then
            table.insert(children, ui.text{ text = err, size = "xs", tone = "danger" })
        end
    end

    table.insert(children, ui.text{ text = "Gates", size = "xs", weight = "semibold" })
    table.insert(children, ui.bind_list{
        source = "/project-pipelines.pipeline_gate",
        where = { step_id = step.id },
        item_template = edit_gate_template(state),
    })

    return view.panel{ ui.stack{ direction = "vertical", gap = "3", children = children } }
end

function M.edit(view_state, ctx)
    local params = view_state and view_state.params or {}
    local pipeline = repo.get_pipeline(params.pipeline_id)
    if not pipeline then
        return view.panel{ ui.text{ text = "Pipeline not found", tone = "danger" } }
    end

    local state = actions.feedback(ctx)
    local steps = repo.pipeline_steps(pipeline.id)
    local children = {
        view.row{
            ui.button{
                label = "Back",
                icon = "arrow-left",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ctx.path("/pipelines") }),
            },
            ui.text{ text = "Edit Pipeline", size = "lg", weight = "semibold" },
        },
        edit_pipeline_fields(pipeline, state),
        ui.text{ text = "Steps", size = "md", weight = "semibold" },
    }
    for _, step in ipairs(steps) do
        table.insert(children, edit_step(step, steps, state))
    end

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

return M
