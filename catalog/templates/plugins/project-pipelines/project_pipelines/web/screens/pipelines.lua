-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/pipelines.lua
-- @scope device
-- @version 1.1.0

local repo = require("project_pipelines.repo")
local source_definitions = require("project_pipelines.source_definitions")
local view = require("project_pipelines.web.ui")
local actions = require("project_pipelines.web.actions")

local M = {}

local function pipeline_card_template()
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
        view.row{
            ui.text{ text = ui.bind("@/name"), size = "sm", weight = "semibold" },
            view.badge(ui.bind("@/step_count_label"), "muted"),
            ui.button{
                id = ui.bind("@/id"),
                label = "Edit",
                icon = "pencil-square",
                variant = "ghost",
                action = ui.action("botster.nav.open", { path = ui.bind("@/edit_path") }),
            },
        },
        ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
        ui.bind_if("@/version_label", view.badge(ui.bind("@/version_label"), "accent")),
        ui.text{ text = ui.bind("@/step_summary"), size = "xs", tone = "muted" },
    } } }
end

function M.index(_view_state, ctx)
    local children = {
        view.page_header{
            title = "Pipeline Definitions",
            back_id = "pipeline-index-back",
            back_path = ctx.path("/"),
            actions = {
                ui.button{
                    id = "pipeline-index-archived",
                    label = "Archived",
                    icon = "archive-box",
                    variant = "ghost",
                    action = ui.action("botster.nav.open", { path = ctx.path("/pipelines/archived") }),
                },
            },
        },
        ui.bind_list{
            source = "/project-pipelines.pipeline",
            where = { active = true },
            item_template = pipeline_card_template(),
            empty_template = view.empty(
                "No pipelines yet",
                "Ask an agent to create one with the Project Pipelines MCP tools.",
                "queue-list"
            ),
        },
    }

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

function M.archived(_view_state, ctx)
    return ui.stack{ direction = "vertical", gap = "4", children = {
        view.page_header{
            title = "Archived Pipeline Definitions",
            back_id = "pipeline-archived-back",
            back_path = ctx.path("/pipelines"),
        },
        ui.bind_list{
            source = "/project-pipelines.pipeline",
            where = { archived = true },
            item_template = pipeline_card_template(),
            empty_template = view.empty(
                "No archived pipelines",
                "Retired pipeline definitions will appear here after they are archived.",
                "archive-box"
            ),
        },
    } }
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
    table.insert(children, ui.select{
        id = "pipeline-" .. pipeline.id .. "-merge-policy",
        label = "Merge policy",
        value = pipeline.merge_policy or "direct",
        options = {
            { value = "direct", label = "Merge directly to main" },
            { value = "pr", label = "Open PR with Botster MCP" },
        },
        on_change = view.field_action("project_pipelines.update_pipeline_field", {
            pipeline_id = pipeline.id,
            field = "merge_policy",
        }),
    })
    local merge_policy_error = feedback_error(state, pipeline.id, "merge_policy")
    if merge_policy_error then
        table.insert(children, ui.text{ text = merge_policy_error, size = "xs", tone = "danger" })
    end
    table.insert(children, ui.text_input{
        id = "pipeline-" .. pipeline.id .. "-version-label",
        label = "Version",
        placeholder = pipeline.version_label or "",
        on_change = view.field_action("project_pipelines.update_pipeline_field", {
            pipeline_id = pipeline.id,
            field = "version_label",
        }),
    })
    local version_error = feedback_error(state, pipeline.id, "version_label")
    if version_error then
        table.insert(children, ui.text{ text = version_error, size = "xs", tone = "danger" })
    end
    table.insert(children, ui.text_input{
        id = "pipeline-" .. pipeline.id .. "-replacement-pipeline",
        label = "Replacement pipeline ID",
        placeholder = pipeline.replacement_pipeline_id or "",
        on_change = view.field_action("project_pipelines.update_pipeline_field", {
            pipeline_id = pipeline.id,
            field = "replacement_pipeline_id",
        }),
    })
    local replacement_error = feedback_error(state, pipeline.id, "replacement_pipeline_id")
    if replacement_error then
        table.insert(children, ui.text{ text = replacement_error, size = "xs", tone = "danger" })
    end
    table.insert(children, ui.text_input{
        id = "pipeline-" .. pipeline.id .. "-supersedes-pipeline",
        label = "Supersedes pipeline ID",
        placeholder = pipeline.supersedes_pipeline_id or "",
        on_change = view.field_action("project_pipelines.update_pipeline_field", {
            pipeline_id = pipeline.id,
            field = "supersedes_pipeline_id",
        }),
    })
    local supersedes_error = feedback_error(state, pipeline.id, "supersedes_pipeline_id")
    if supersedes_error then
        table.insert(children, ui.text{ text = supersedes_error, size = "xs", tone = "danger" })
    end
    table.insert(children, ui.checkbox{
        id = "pipeline-" .. pipeline.id .. "-archived",
        label = "Archived",
        selected = pipeline.archived_at ~= nil and tostring(pipeline.archived_at) ~= "",
        on_change = view.field_action("project_pipelines.update_pipeline_field", {
            pipeline_id = pipeline.id,
            field = "archived",
        }),
    })
    local archived_error = feedback_error(state, pipeline.id, "archived")
    if archived_error then
        table.insert(children, ui.text{ text = archived_error, size = "xs", tone = "danger" })
    end
    return view.panel{
        ui.stack{ direction = "vertical", gap = "3", children = children },
    }
end

local function edit_gate(gate, state)
    local children = {
        view.row{
            view.badge(gate.kind, "muted"),
            ui.text{ text = gate.id, size = "xs", tone = "muted" },
        },
        ui.textarea{
            id = "gate-" .. gate.id .. "-prompt",
            label = "Gate prompt",
            placeholder = gate.prompt or "",
            on_change = view.field_action("project_pipelines.update_gate_field", {
                gate_id = gate.id,
                field = "prompt",
            }),
        },
    }
    local prompt_error = feedback_error(state, gate.id, "prompt")
    if prompt_error then
        table.insert(children, ui.text{ text = prompt_error, size = "xs", tone = "danger" })
    end

    if gate.kind == "command" then
        table.insert(children, ui.text_input{
            id = "gate-" .. gate.id .. "-command",
            label = "Command",
            placeholder = gate.command or "",
            on_change = view.field_action("project_pipelines.update_gate_field", {
                gate_id = gate.id,
                field = "command",
            }),
        })
        local err = feedback_error(state, gate.id, "command")
        if err then
            table.insert(children, ui.text{ text = err, size = "xs", tone = "danger" })
        end
    end

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

    local gates = repo.step_gates(step.id)
    if #gates > 0 then
        table.insert(children, ui.text{ text = "Gates", size = "xs", weight = "semibold" })
        for _, gate in ipairs(gates) do
            table.insert(children, edit_gate(gate, state))
        end
    end

    return view.panel{ ui.stack{ direction = "vertical", gap = "3", children = children } }
end

local function sourced_gate(gate)
    local children = {
        view.row{
            view.badge(gate.kind, "muted"),
            ui.text{ text = gate.id, size = "xs", tone = "muted" },
        },
        ui.text{ text = gate.prompt or "", size = "xs" },
    }
    if gate.command and gate.command ~= "" then
        table.insert(children, ui.text{ text = gate.command, size = "xs", tone = "muted" })
    end
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = children } }
end

local function sourced_step(step, state)
    local children = {
        view.row{
            view.badge(step.position, "muted"),
            ui.text{ text = step.name, size = "sm", weight = "semibold" },
            view.badge(step.kind, "muted"),
        },
        ui.text{ text = step.prompt or "", size = "xs" },
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
        table.insert(children, ui.text{
            text = "Agent selection is device-local and is preserved when package-owned structure is reconciled.",
            size = "xs",
            tone = "muted",
        })
        local agent_error = feedback_error(state, step.id, "agent_name")
        if agent_error then
            table.insert(children, ui.text{ text = agent_error, size = "xs", tone = "danger" })
        end
    elseif step.command and step.command ~= "" then
        table.insert(children, ui.text{ text = step.command, size = "xs", tone = "muted" })
    end

    table.insert(children, ui.text{ text = "Transitions", size = "xs", weight = "semibold" })
    for _, transition in ipairs({
        { field = "next_step_id", label = "Default next step" },
        { field = "on_approved_step_id", label = "On review approved" },
        { field = "on_changes_requested_step_id", label = "On changes requested" },
        { field = "on_blocked_step_id", label = "On blocked" },
    }) do
        if step[transition.field] and step[transition.field] ~= "" then
            table.insert(children, ui.text{
                text = transition.label .. ": " .. step[transition.field],
                size = "xs",
                tone = "muted",
            })
        end
    end

    local gates = repo.step_gates(step.id)
    if #gates > 0 then
        table.insert(children, ui.text{ text = "Gates", size = "xs", weight = "semibold" })
        for _, gate in ipairs(gates) do
            table.insert(children, sourced_gate(gate))
        end
    end

    return view.panel{ ui.stack{ direction = "vertical", gap = "3", children = children } }
end

local function sourced_pipeline_fields(pipeline)
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = {
        view.row{
            view.badge("Package-owned", "accent"),
            view.badge("Read-only structure", "muted"),
        },
        ui.text{ text = pipeline.description or "", size = "xs" },
        ui.text{ text = "Merge policy: " .. tostring(pipeline.merge_policy or "direct"), size = "xs", tone = "muted" },
        ui.text{
            text = "Structural changes come from " .. source_definitions.source_path(),
            size = "xs",
            tone = "muted",
        },
    } } }
end

function M.edit(view_state, ctx)
    local params = view_state and view_state.params or {}
    local pipeline = repo.get_pipeline(params.pipeline_id)
    if not pipeline then
        return view.panel{ ui.text{ text = "Pipeline not found", tone = "danger" } }
    end

    local state = actions.feedback(ctx)
    local steps = repo.pipeline_steps(pipeline.id)
    local meta = {
        view.badge(pipeline.id, "muted"),
        view.badge(tostring(#steps) .. " steps", "muted"),
    }
    if pipeline.version_label and pipeline.version_label ~= "" then
        table.insert(meta, view.badge(pipeline.version_label, "accent"))
    end
    if pipeline.archived_at then
        table.insert(meta, view.badge("archived", "muted"))
    end

    local is_sourced = source_definitions.is_sourced_pipeline_id(pipeline.id)
    local children = {
        view.page_header{
            title = is_sourced and "Pipeline Definition" or "Edit Pipeline",
            back_id = "pipeline-" .. pipeline.id .. "-back",
            back_path = ctx.path("/pipelines"),
            meta = meta,
        },
        is_sourced and sourced_pipeline_fields(pipeline) or edit_pipeline_fields(pipeline, state),
        ui.text{ text = "Steps", size = "md", weight = "semibold" },
    }
    for _, step in ipairs(steps) do
        table.insert(children, is_sourced and sourced_step(step, state) or edit_step(step, steps, state))
    end

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

return M
