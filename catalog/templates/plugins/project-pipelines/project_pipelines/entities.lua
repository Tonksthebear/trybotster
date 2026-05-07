-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/entities.lua
-- @scope device
-- @version 1.0.0

local db = require("project_pipelines.db")
local util = require("project_pipelines.util")

local OWNER = "project-pipelines"

local M = {}

M.types = {
    ticket = OWNER .. ".ticket",
    project = OWNER .. ".project",
    project_target = OWNER .. ".project_target",
    ticket_dependency = OWNER .. ".ticket_dependency",
    pipeline = OWNER .. ".pipeline",
    pipeline_step = OWNER .. ".pipeline_step",
    pipeline_gate = OWNER .. ".pipeline_gate",
    run = OWNER .. ".run",
    run_step = OWNER .. ".run_step",
    gate_result = OWNER .. ".gate_result",
    review = OWNER .. ".review",
    finding = OWNER .. ".finding",
    artifact = OWNER .. ".artifact",
    question = OWNER .. ".question",
    event = OWNER .. ".event",
}

local function rows(sql, ...)
    local params = { ... }
    local result
    if #params == 0 then
        result = db:eval(sql)
    elseif #params == 1 then
        result = db:eval(sql, params[1])
    else
        result = db:eval(sql, params)
    end
    if type(result) == "table" then
        return result
    end
    return {}
end

local function copy(row)
    return util.copy(row or {})
end

local function decode(value, fallback)
    if type(value) == "string" then
        return util.decode(value, fallback or {})
    end
    return value or fallback or {}
end

local function with_repo()
    local ok, repo = pcall(require, "project_pipelines.repo")
    if ok then
        return repo
    end
    return nil
end

local function with_view()
    local ok, view = pcall(require, "project_pipelines.web.ui")
    if ok then
        return view
    end
    return nil
end

local function target_label(view, target_id, target_path)
    if view and view.target_label then
        return view.target_label(target_id, target_path)
    end
    return target_id or target_path or "No target"
end

local function index_by_id(items)
    local out = {}
    for _, item in ipairs(items or {}) do
        if item.id then
            out[item.id] = item
        end
    end
    return out
end

local function grouped_by(items, key)
    local out = {}
    for _, item in ipairs(items or {}) do
        local value = item[key]
        if value then
            out[value] = out[value] or {}
            table.insert(out[value], item)
        end
    end
    return out
end

local function build_ticket_notification_counts(repo, view)
    if not repo or not view then
        return {}
    end

    local uuid_by_ticket = {}
    local seen_by_ticket = {}
    local all_uuids = {}

    local function add(ticket_id, uuid)
        if not ticket_id or not uuid or uuid == "" then
            return
        end
        local seen = seen_by_ticket[ticket_id]
        if not seen then
            seen = {}
            seen_by_ticket[ticket_id] = seen
        end
        if seen[uuid] then
            return
        end
        seen[uuid] = true
        uuid_by_ticket[ticket_id] = uuid_by_ticket[ticket_id] or {}
        table.insert(uuid_by_ticket[ticket_id], uuid)
        all_uuids[uuid] = true
    end

    for _, row in ipairs(rows([[SELECT r.ticket_id, rs.agent_session_uuid
                                FROM run_steps rs
                                JOIN runs r ON r.id = rs.run_id
                                WHERE rs.agent_session_uuid IS NOT NULL
                                  AND rs.agent_session_uuid != '']])) do
        add(row.ticket_id, row.agent_session_uuid)
    end

    for _, event in ipairs(rows([[SELECT ticket_id, kind, payload
                                  FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind IN ('ticket.merge_requested',
                                                 'ticket.merge_agent_linked',
                                                 'question.agent_linked')]])) do
        local payload = decode(event.payload, {})
        add(event.ticket_id, payload.session_uuid)
    end

    local Agent = nil
    local ok, mod = pcall(require, "lib.agent")
    if ok then
        Agent = mod
    end

    local notified = {}
    if Agent then
        for uuid in pairs(all_uuids) do
            if view.session_has_notification then
                notified[uuid] = view.session_has_notification(uuid) == true
            else
                local session = Agent.get(uuid)
                if session and session.info then
                    local ok_info, info = pcall(session.info, session)
                    notified[uuid] = ok_info and info and info.notification == true
                end
            end
        end
    end

    local counts = {}
    for ticket_id, uuids in pairs(uuid_by_ticket) do
        local count = 0
        for _, uuid in ipairs(uuids) do
            if notified[uuid] then
                count = count + 1
            end
        end
        counts[ticket_id] = count
    end
    return counts
end

local function ticket_entities(tickets)
    local repo = with_repo()
    local view = with_view()
    local runs_by_ticket = grouped_by(rows("SELECT * FROM runs ORDER BY ticket_id ASC, created_at DESC, id DESC"), "ticket_id")
    local steps_by_id = index_by_id(rows("SELECT * FROM pipeline_steps"))
    local notification_counts = build_ticket_notification_counts(repo, view)
    local out = {}
    for _, ticket in ipairs(tickets or {}) do
        local entity = copy(ticket)
        local runs = runs_by_ticket[ticket.id] or {}
        local latest = runs[1]
        local current_step = latest and latest.current_step_id and steps_by_id[latest.current_step_id] or nil
        local notifications = notification_counts[ticket.id] or 0
        entity.target_label = target_label(view, ticket.target_id, ticket.target_path)
        entity.standalone = ticket.project_id == nil or ticket.project_id == ""
        entity.run_count = #runs
        entity.run_count_label = string.format("%d run%s", #runs, #runs == 1 and "" or "s")
        entity.latest_run_id = latest and latest.id or nil
        entity.latest_run_status = latest and latest.status or nil
        entity.latest_run_badge = latest and (latest.status == "done" and "complete" or latest.status == "blocked" and "blocked" or "in progress") or "ready"
        entity.latest_run_tone = latest and (latest.status == "done" and "success" or latest.status == "blocked" and "danger" or "accent") or "muted"
        entity.tail_label = latest and (current_step and ("Working: " .. current_step.name) or "In pipeline") or "No runs yet"
        entity.notification_count = notifications
        entity.notification_label = tostring(notifications) .. " notification"
        entity.secondary_badge = notifications > 0 and (tostring(notifications) .. " notification") or entity.target_label
        entity.secondary_badge_tone = notifications > 0 and "danger" or "muted"
        entity.path = "/pipelines/tickets/" .. ticket.id
        out[#out + 1] = entity
    end
    return out
end

local function ticket_entity(ticket)
    local entity = copy(ticket)
    local repo = with_repo()
    local view = with_view()
    local runs = repo and repo.ticket_runs(ticket.id) or {}
    local latest = runs[1]
    local current_step = latest and latest.current_step_id and repo and repo.get_step(latest.current_step_id) or nil
    local notifications = view and repo and view.ticket_notification_count(ticket.id, repo) or 0
    entity.target_label = target_label(view, ticket.target_id, ticket.target_path)
    entity.standalone = ticket.project_id == nil or ticket.project_id == ""
    entity.run_count = #runs
    entity.run_count_label = string.format("%d run%s", #runs, #runs == 1 and "" or "s")
    entity.latest_run_id = latest and latest.id or nil
    entity.latest_run_status = latest and latest.status or nil
    entity.latest_run_badge = latest and (latest.status == "done" and "complete" or latest.status == "blocked" and "blocked" or "in progress") or "ready"
    entity.latest_run_tone = latest and (latest.status == "done" and "success" or latest.status == "blocked" and "danger" or "accent") or "muted"
    entity.tail_label = latest and (current_step and ("Working: " .. current_step.name) or "In pipeline") or "No runs yet"
    entity.notification_count = notifications
    entity.notification_label = tostring(notifications) .. " notification"
    entity.secondary_badge = notifications > 0 and (tostring(notifications) .. " notification") or entity.target_label
    entity.secondary_badge_tone = notifications > 0 and "danger" or "muted"
    entity.path = "/pipelines/tickets/" .. ticket.id
    return entity
end

local function project_entity(project)
    local entity = copy(project)
    local view = with_view()
    entity.status_tone = view and view.status_tone(project.status) or "muted"
    entity.path = "/pipelines/projects/" .. project.id
    return entity
end

local function pipeline_entity(pipeline)
    local entity = copy(pipeline)
    local step_count = #rows("SELECT id FROM pipeline_steps WHERE pipeline_id = ?", pipeline.id)
    entity.step_count = step_count
    entity.step_count_label = string.format("%d step%s", step_count, step_count == 1 and "" or "s")
    entity.edit_path = "/pipelines/pipelines/" .. pipeline.id .. "/edit"
    return entity
end

local function pipeline_entities(pipelines)
    local counts = {}
    for _, row in ipairs(rows("SELECT pipeline_id, COUNT(*) AS count FROM pipeline_steps GROUP BY pipeline_id")) do
        counts[row.pipeline_id] = tonumber(row.count or 0) or 0
    end
    local out = {}
    for _, pipeline in ipairs(pipelines or {}) do
        local entity = copy(pipeline)
        local step_count = counts[pipeline.id] or 0
        entity.step_count = step_count
        entity.step_count_label = string.format("%d step%s", step_count, step_count == 1 and "" or "s")
        entity.edit_path = "/pipelines/pipelines/" .. pipeline.id .. "/edit"
        out[#out + 1] = entity
    end
    return out
end

local function run_entities(runs)
    local tickets_by_id = index_by_id(rows("SELECT id, title FROM tickets"))
    local pipelines_by_id = index_by_id(rows("SELECT id, name FROM pipelines"))
    local out = {}
    for _, run in ipairs(runs or {}) do
        local entity = copy(run)
        local ticket = tickets_by_id[run.ticket_id]
        local pipeline = pipelines_by_id[run.pipeline_id]
        entity.ticket_title = ticket and ticket.title or run.ticket_id
        entity.pipeline_name = pipeline and pipeline.name or run.pipeline_id
        entity.label = entity.ticket_title .. " - " .. entity.pipeline_name .. " (" .. tostring(run.status) .. ")"
        entity.path = "/pipelines/runs/" .. run.id
        out[#out + 1] = entity
    end
    return out
end

local function run_entity(run)
    local entity = copy(run)
    local ticket = rows("SELECT * FROM tickets WHERE id = ? LIMIT 1", run.ticket_id)[1]
    local pipeline = rows("SELECT * FROM pipelines WHERE id = ? LIMIT 1", run.pipeline_id)[1]
    entity.ticket_title = ticket and ticket.title or run.ticket_id
    entity.pipeline_name = pipeline and pipeline.name or run.pipeline_id
    entity.label = entity.ticket_title .. " - " .. entity.pipeline_name .. " (" .. tostring(run.status) .. ")"
    entity.path = "/pipelines/runs/" .. run.id
    return entity
end

local function run_step_entity(run_step)
    local entity = copy(run_step)
    local step = rows("SELECT * FROM pipeline_steps WHERE id = ? LIMIT 1", run_step.step_id)[1]
    local run = rows("SELECT * FROM runs WHERE id = ? LIMIT 1", run_step.run_id)[1]
    if step then
        entity.name = step.name
        entity.kind = step.kind
        entity.position = step.position
        entity.agent_name = step.agent_name
        entity.prompt = step.prompt
        entity.command = step.command
    end
    if run then
        entity.ticket_id = run.ticket_id
        entity.pipeline_id = run.pipeline_id
    end
    return entity
end

local function run_step_entities(run_steps)
    local steps_by_id = index_by_id(rows("SELECT * FROM pipeline_steps"))
    local runs_by_id = index_by_id(rows("SELECT id, ticket_id, pipeline_id FROM runs"))
    local out = {}
    for _, run_step in ipairs(run_steps or {}) do
        local entity = copy(run_step)
        local step = steps_by_id[run_step.step_id]
        local run = runs_by_id[run_step.run_id]
        if step then
            entity.name = step.name
            entity.kind = step.kind
            entity.position = step.position
            entity.agent_name = step.agent_name
            entity.prompt = step.prompt
            entity.command = step.command
        end
        if run then
            entity.ticket_id = run.ticket_id
            entity.pipeline_id = run.pipeline_id
        end
        out[#out + 1] = entity
    end
    return out
end

local function pipeline_gate_entity(gate)
    local entity = copy(gate)
    entity.required_fields = decode(gate.required_fields, {})
    return entity
end

local function gate_result_entity(result)
    local entity = copy(result)
    entity.evidence = decode(result.evidence, {})
    return entity
end

local function question_entity(question)
    local entity = copy(question)
    local ticket = rows("SELECT id, title FROM tickets WHERE id = ? LIMIT 1", question.ticket_id)[1]
    entity.ticket_title = ticket and ticket.title or question.ticket_id
    entity.path = "/pipelines/tickets/" .. tostring(question.ticket_id or "")
    entity.kind_label = question.kind == "agent" and "agent" or "human"
    entity.blocking_label = question.blocking == 1 and "blocking" or "open"
    entity.blocking_tone = question.blocking == 1 and "danger" or "accent"
    return entity
end

local function artifact_entity(artifact)
    local entity = copy(artifact)
    entity.payload = decode(artifact.payload, {})
    return entity
end

local function event_entity(event)
    local entity = copy(event)
    entity.payload = decode(event.payload, {})
    return entity
end

local ENTITY = {
    [M.types.ticket] = {
        all = function()
            return ticket_entities(rows("SELECT * FROM tickets ORDER BY updated_at DESC, created_at DESC"))
        end,
        one = ticket_entity,
    },
    [M.types.project] = {
        all = function()
            local out = {}
            for _, project in ipairs(rows("SELECT * FROM projects ORDER BY updated_at DESC, created_at DESC")) do
                out[#out + 1] = project_entity(project)
            end
            return out
        end,
        one = project_entity,
    },
    [M.types.project_target] = {
        all = function() return rows("SELECT * FROM project_targets ORDER BY created_at ASC, id ASC") end,
        one = copy,
    },
    [M.types.ticket_dependency] = {
        all = function()
            return rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                          FROM ticket_dependencies td
                          LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                          ORDER BY td.created_at ASC, td.id ASC]])
        end,
        one = copy,
    },
    [M.types.pipeline] = {
        all = function()
            return pipeline_entities(rows("SELECT * FROM pipelines ORDER BY created_at ASC, id ASC"))
        end,
        one = pipeline_entity,
    },
    [M.types.pipeline_step] = {
        all = function() return rows("SELECT * FROM pipeline_steps ORDER BY pipeline_id ASC, position ASC, created_at ASC, id ASC") end,
        one = copy,
    },
    [M.types.pipeline_gate] = {
        all = function()
            local out = {}
            for _, gate in ipairs(rows("SELECT * FROM pipeline_gates ORDER BY step_id ASC, created_at ASC, id ASC")) do
                out[#out + 1] = pipeline_gate_entity(gate)
            end
            return out
        end,
        one = pipeline_gate_entity,
    },
    [M.types.run] = {
        all = function()
            return run_entities(rows("SELECT * FROM runs ORDER BY updated_at DESC, created_at DESC, id DESC"))
        end,
        one = run_entity,
    },
    [M.types.run_step] = {
        all = function()
            return run_step_entities(rows("SELECT * FROM run_steps ORDER BY run_id ASC, COALESCE(sequence, 0) ASC, created_at ASC, id ASC"))
        end,
        one = run_step_entity,
    },
    [M.types.gate_result] = {
        all = function()
            local out = {}
            for _, result in ipairs(rows("SELECT * FROM gate_results ORDER BY created_at DESC, id DESC")) do
                out[#out + 1] = gate_result_entity(result)
            end
            return out
        end,
        one = gate_result_entity,
    },
    [M.types.review] = {
        all = function() return rows("SELECT * FROM reviews ORDER BY created_at DESC, id DESC") end,
        one = copy,
    },
    [M.types.finding] = {
        all = function() return rows("SELECT * FROM review_findings ORDER BY created_at DESC, id DESC") end,
        one = copy,
    },
    [M.types.artifact] = {
        all = function()
            local out = {}
            for _, artifact in ipairs(rows("SELECT * FROM artifacts ORDER BY created_at DESC, id DESC")) do
                out[#out + 1] = artifact_entity(artifact)
            end
            return out
        end,
        one = artifact_entity,
    },
    [M.types.question] = {
        all = function()
            local out = {}
            for _, question in ipairs(rows("SELECT * FROM questions ORDER BY updated_at DESC, created_at DESC, id DESC")) do
                out[#out + 1] = question_entity(question)
            end
            return out
        end,
        one = question_entity,
    },
    [M.types.event] = {
        all = function()
            local out = {}
            for _, event in ipairs(rows("SELECT * FROM events ORDER BY created_at DESC, id DESC LIMIT 200")) do
                out[#out + 1] = event_entity(event)
            end
            return out
        end,
        one = event_entity,
    },
}

local function opts()
    return { owner_plugin = OWNER }
end

function M.register()
    local EB = require("lib.entity_broadcast")
    for _, entity_type in pairs(M.types) do
        local spec = ENTITY[entity_type]
        EB.register(entity_type, {
            id_field = "id",
            owner_plugin = OWNER,
            all = spec.all,
        })
    end
end

function M.snapshot(entity_type)
    local spec = ENTITY[entity_type]
    if not spec then
        return
    end
    local Hub = require("lib.hub")
    Hub.get():entity_snapshot(entity_type, spec.all(), opts())
end

function M.publish_snapshots()
    for _, entity_type in pairs(M.types) do
        M.snapshot(entity_type)
    end
end

function M.upsert(entity_type, row)
    if not row or type(row.id) ~= "string" or row.id == "" then
        return
    end
    local spec = ENTITY[entity_type]
    if not spec then
        return
    end
    local Hub = require("lib.hub")
    Hub.get():entity_upsert(entity_type, spec.one(row), opts())
end

function M.remove(entity_type, id)
    if util.is_blank(id) then
        return
    end
    local Hub = require("lib.hub")
    Hub.get():entity_remove(entity_type, id, opts())
end

return M
