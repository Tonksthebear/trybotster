-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/actions.lua
-- @scope device
-- @version 1.0.0

local action = require("lib.action")
local repo = require("project_pipelines.repo")
local engine = require("project_pipelines.engine")

local M = {}
local drafts = {}
local feedback = {}

local ALLOWED_FIELDS = {
    pipeline = { name = true, description = true },
    step = {
        name = true,
        prompt = true,
        agent_name = true,
        command = true,
        position = true,
        next_step_id = true,
        on_approved_step_id = true,
        on_changes_requested_step_id = true,
        on_blocked_step_id = true,
    },
    gate = { prompt = true, command = true, required_fields = true },
}

local function value_payload(envelope)
    return envelope and envelope.payload or {}
end

local function refresh(ctx)
    engine.refresh_surfaces(ctx)
end

local function draft_key(ctx)
    return (ctx and ctx.sub_id) or "default"
end

local function current_draft(ctx)
    local key = draft_key(ctx)
    drafts[key] = drafts[key] or {}
    return drafts[key]
end

local function current_feedback(ctx)
    local key = draft_key(ctx)
    feedback[key] = feedback[key] or {}
    return feedback[key]
end

function M.feedback(ctx)
    return current_feedback(ctx)
end

function M.draft(ctx)
    return current_draft(ctx)
end

local function allowed(kind, field)
    return field and ALLOWED_FIELDS[kind] and ALLOWED_FIELDS[kind][field] == true
end

local function field_error_key(id, field)
    return tostring(id or "") .. ":" .. tostring(field or "")
end

local function set_field_error(ctx, id, field, err)
    local status = current_feedback(ctx)
    status.field_errors = status.field_errors or {}
    status.field_errors[field_error_key(id, field)] = err and tostring(err) or nil
end

function M.register()
    action.on("project_pipelines.update_ticket_draft", "project_pipelines.update_ticket_draft", function(envelope, ctx)
        local payload = value_payload(envelope)
        local draft = current_draft(ctx)
        if payload.field == "title" or payload.field == "description" or payload.field == "target_id" or payload.field == "project_id" then
            draft[payload.field] = payload.value or ""
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.new_ticket_for_project", "project_pipelines.new_ticket_for_project", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.project_id then
            local draft = current_draft(ctx)
            draft.project_id = payload.project_id
        end
        refresh(ctx)
        return action.HANDLED
    end)

    action.on("project_pipelines.create_ticket", "project_pipelines.create_ticket", function(envelope, ctx)
        local payload = value_payload(envelope)
        local key = draft_key(ctx)
        local draft = drafts[key] or {}
        local title = draft.title
        if not title or title:match("^%s*$") then
            title = "Untitled ticket"
        end
        local target_id = draft.target_id
        local target_path = nil
        if spawn_targets and spawn_targets.get and target_id and target_id ~= "" then
            local target = spawn_targets.get(target_id)
            target_path = target and target.path or nil
        end
        if not target_id or target_id == "" then
            refresh(ctx)
            return action.result{ ok = false, error = "Select a spawn target before creating the ticket." }
        end
        local project_id = (draft.project_id and draft.project_id ~= "" and draft.project_id)
            or (payload.project_id and payload.project_id ~= "" and payload.project_id)
            or nil
        local ticket = repo.create_ticket{
            title = title,
            description = draft.description or "",
            project_id = project_id,
            target_id = target_id,
            target_path = target_path,
        }
        drafts[key] = {}
        refresh(ctx)
        return action.result{
            message = "Ticket created.",
            navigate = { label = "Open ticket", path = "/pipelines/tickets/" .. ticket.id },
        }
    end)

    action.on("project_pipelines.update_project_draft", "project_pipelines.update_project_draft", function(envelope, ctx)
        local payload = value_payload(envelope)
        local draft = current_draft(ctx)
        if payload.field == "name" or payload.field == "description" or payload.field == "target_id" then
            draft["project_" .. payload.field] = payload.value or ""
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.create_project", "project_pipelines.create_project", function(_envelope, ctx)
        local draft = current_draft(ctx)
        local name = draft.project_name
        if not name or name:match("^%s*$") then
            name = "Untitled project"
        end
        local target_id = draft.project_target_id
        local target_path = nil
        if spawn_targets and spawn_targets.get and target_id and target_id ~= "" then
            local target = spawn_targets.get(target_id)
            target_path = target and target.path or nil
        end
        local project = repo.create_project{
            name = name,
            description = draft.project_description or "",
            target_id = target_id ~= "" and target_id or nil,
            target_path = target_path,
        }
        draft.project_name = nil
        draft.project_description = nil
        draft.project_target_id = nil
        refresh(ctx)
        return action.result{
            message = "Project created.",
            navigate = { label = "Open project", path = "/pipelines/projects/" .. project.id },
        }
    end)

    action.on("project_pipelines.update_pipeline_field", "project_pipelines.update_pipeline_field", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.pipeline_id and allowed("pipeline", payload.field) then
            local ok, err = pcall(repo.update_pipeline, payload.pipeline_id, { [payload.field] = payload.value or "" })
            set_field_error(ctx, payload.pipeline_id, payload.field, ok and nil or err)
            refresh(ctx)
        elseif payload.field then
            log.warn("[project-pipelines] rejected pipeline field update: " .. tostring(payload.field))
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.update_step_field", "project_pipelines.update_step_field", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.step_id and allowed("step", payload.field) then
            local ok, err = pcall(repo.update_step, payload.step_id, { [payload.field] = payload.value or "" })
            set_field_error(ctx, payload.step_id, payload.field, ok and nil or err)
            refresh(ctx)
        elseif payload.field then
            log.warn("[project-pipelines] rejected step field update: " .. tostring(payload.field))
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.update_gate_field", "project_pipelines.update_gate_field", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.gate_id and allowed("gate", payload.field) then
            local ok, err = pcall(repo.update_gate, payload.gate_id, { [payload.field] = payload.value or "" })
            set_field_error(ctx, payload.gate_id, payload.field, ok and nil or err)
            refresh(ctx)
        elseif payload.field then
            log.warn("[project-pipelines] rejected gate field update: " .. tostring(payload.field))
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.start_ticket_pipeline", "project_pipelines.start_ticket_pipeline", function(envelope, ctx)
        local payload = value_payload(envelope)
        local started = nil
        if payload.ticket_id and payload.pipeline_id then
            local ok, result = pcall(engine.start_run, {
                ticket_id = payload.ticket_id,
                pipeline_id = payload.pipeline_id,
                workspace_name = payload.workspace_name or "Pipelines",
            })
            refresh(ctx)
            if not ok then
                return action.result{ ok = false, error = tostring(result) }
            end
            started = result
        end
        return action.result{
            message = "Pipeline started.",
            navigate = started and started.run and { label = "Open run", path = "/pipelines/runs/" .. started.run.id } or nil,
        }
    end)

    action.on("project_pipelines.close_ticket", "project_pipelines.close_ticket", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.ticket_id then
            local ok, err = pcall(engine.close_ticket, payload.ticket_id, { merge_confirmed = payload.merge_confirmed == true })
            refresh(ctx)
            if not ok then
                return action.result{ ok = false, error = tostring(err) }
            end
        end
        return action.result{ message = "Ticket closed." }
    end)

    action.on("project_pipelines.update_dependency_draft", "project_pipelines.update_dependency_draft", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.ticket_id then
            current_draft(ctx)["dependency_" .. payload.ticket_id] = payload.value or ""
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.add_ticket_dependency", "project_pipelines.add_ticket_dependency", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.ticket_id then
            local depends_on_ticket_id = current_draft(ctx)["dependency_" .. payload.ticket_id]
            local ok, result = pcall(repo.add_ticket_dependency, payload.ticket_id, depends_on_ticket_id)
            if ok then
                current_draft(ctx)["dependency_" .. payload.ticket_id] = nil
            end
            refresh(ctx)
            if not ok then
                return action.result{ ok = false, error = tostring(result) }
            end
        end
        return action.result{ message = "Dependency added." }
    end)

    action.on("project_pipelines.remove_ticket_dependency", "project_pipelines.remove_ticket_dependency", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.dependency_id then
            local ok, result = pcall(repo.remove_ticket_dependency, payload.dependency_id)
            refresh(ctx)
            if not ok then
                return action.result{ ok = false, error = tostring(result) }
            end
        end
        return action.result{ message = "Dependency removed." }
    end)

    action.on("project_pipelines.request_merge", "project_pipelines.request_merge", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.ticket_id then
            local ok, result = pcall(engine.request_merge, {
                ticket_id = payload.ticket_id,
                agent_name = payload.agent_name,
                workspace_name = payload.workspace_name or "Project Management",
            }, {})
            refresh(ctx)
            if not ok then
                return action.result{ ok = false, error = tostring(result) }
            end
        end
        return action.result{ message = "Merge agent requested." }
    end)

    action.on("project_pipelines.update_question_answer", "project_pipelines.update_question_answer", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.question_id then
            local draft = current_draft(ctx)
            draft["answer_" .. payload.question_id] = payload.value or ""
        end
        return action.HANDLED
    end)

    action.on("project_pipelines.answer_question", "project_pipelines.answer_question", function(envelope, ctx)
        local payload = value_payload(envelope)
        if payload.question_id then
            local draft = current_draft(ctx)
            local answer = draft["answer_" .. payload.question_id] or payload.answer or ""
            local ok, err = pcall(engine.answer_question, {
                question_id = payload.question_id,
                answer = answer,
                status = payload.status or "answered",
            }, {})
            if ok then
                draft["answer_" .. payload.question_id] = nil
            end
            refresh(ctx)
            if not ok then
                return action.result{ ok = false, error = tostring(err) }
            end
        end
        return action.result{ message = "Question answered." }
    end)
end

return M
