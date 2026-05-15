-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/entities.lua
-- @scope device
-- @version 1.1.0

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
    checklist = OWNER .. ".checklist",
    checklist_item = OWNER .. ".checklist_item",
    pr_link = OWNER .. ".pr_link",
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

local function target_label(view, target_id)
    if view and view.target_label then
        return view.target_label(target_id)
    end
    return target_id or "No target"
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

local function ticket_dependency_levels(tickets, dependencies)
    local by_id = {}
    local deps_by_ticket = grouped_by(dependencies or {}, "ticket_id")
    for _, ticket in ipairs(tickets or {}) do
        if ticket.id then
            by_id[ticket.id] = ticket
        end
    end

    local memo = {}
    local visiting = {}
    local function level(ticket_id)
        if memo[ticket_id] then
            return memo[ticket_id]
        end
        if visiting[ticket_id] then
            return 0
        end
        visiting[ticket_id] = true
        local max_dependency_level = -1
        for _, dependency in ipairs(deps_by_ticket[ticket_id] or {}) do
            if by_id[dependency.depends_on_ticket_id] then
                max_dependency_level = math.max(max_dependency_level, level(dependency.depends_on_ticket_id))
            end
        end
        visiting[ticket_id] = nil
        memo[ticket_id] = max_dependency_level + 1
        return memo[ticket_id]
    end

    for _, ticket in ipairs(tickets or {}) do
        level(ticket.id)
    end
    return memo, deps_by_ticket
end

local function latest_merge_events_by_ticket()
    local out = {}
    for _, event in ipairs(rows([[SELECT * FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind IN ('ticket.merge_requested',
                                                 'ticket.merge_agent_linked')
                                  ORDER BY created_at DESC, id DESC]])) do
        if event.ticket_id and not out[event.ticket_id] then
            out[event.ticket_id] = event
        end
    end
    return out
end

local function merge_artifacts_by_run()
    local out = {}
    for _, artifact in ipairs(rows([[SELECT * FROM artifacts
                                     WHERE kind = 'merge'
                                     ORDER BY created_at DESC, id DESC]])) do
        if artifact.run_id and not out[artifact.run_id] then
            out[artifact.run_id] = artifact
        end
    end
    return out
end

local function decorate_ticket_entity(entity, opts)
    opts = opts or {}
    local view = opts.view
    local runs = opts.runs or {}
    local latest = runs[1]
    local current_step = latest and latest.current_step_id and opts.steps_by_id and opts.steps_by_id[latest.current_step_id] or nil
    local dependencies = opts.dependencies or {}
    local dependency_labels = {}
    for _, dependency in ipairs(dependencies) do
        table.insert(dependency_labels, dependency.depends_on_title or dependency.depends_on_ticket_id)
    end

    local notifications = opts.notifications or 0
    local merge_event = opts.merge_event
    local merge_artifact = latest and opts.merge_artifacts_by_run and opts.merge_artifacts_by_run[latest.id] or nil
    local merge_active = entity.status ~= "closed"
        and latest
        and latest.status == "done"
        and merge_event ~= nil
        and merge_artifact == nil
    local merge_payload = merge_event and decode(merge_event.payload, {}) or {}
    local merge_session_uuid = merge_payload.session_uuid

    entity.target_label = target_label(view, entity.target_id)
    entity.status_tone = view and view.status_tone and view.status_tone(entity.status) or "muted"
    entity.status_label = view and view.status_label and view.status_label(entity.status) or tostring(entity.status or "")
    entity.status_state = view and view.status_state and view.status_state(entity.status) or "neutral"
    entity.standalone = entity.project_id == nil or entity.project_id == ""
    entity.run_count = #runs
    entity.run_count_label = string.format("%d run%s", #runs, #runs == 1 and "" or "s")
    entity.latest_run_id = latest and latest.id or nil
    entity.latest_run_status = latest and latest.status or nil
    entity.project_dependency_level = opts.dependency_level or 0
    entity.project_stage_label = "stage " .. tostring((opts.dependency_level or 0) + 1)
    entity.project_sort_key = string.format("%04d:%012d:%s",
        opts.dependency_level or 0,
        tonumber(entity.created_at or 0) or 0,
        tostring(entity.id or ""))
    entity.dependency_summary = #dependency_labels > 0
        and ("Depends on: " .. table.concat(dependency_labels, ", "))
        or "Starts this branch of work."
    entity.merge_active = merge_active and true or false
    entity.merge_session_uuid = merge_session_uuid

    if entity.status == "closed" then
        entity.latest_run_badge = "complete"
        entity.latest_run_tone = "success"
        entity.tail_label = "Complete"
        entity.active_work_label = "Complete"
        entity.active_work_detail = "Ticket is closed."
    elseif merge_active then
        entity.latest_run_badge = util.is_blank(merge_session_uuid) and "merge queued" or "merge active"
        entity.latest_run_tone = "accent"
        entity.tail_label = util.is_blank(merge_session_uuid) and "Merge queued" or "Merge active"
        entity.active_work_label = entity.tail_label
        entity.active_work_detail = "Merge agent is handling final integration."
    elseif latest then
        entity.latest_run_badge = latest.status == "done" and "ready for merge"
            or latest.status == "blocked" and "blocked"
            or "in progress"
        entity.latest_run_tone = latest.status == "done" and "muted"
            or latest.status == "blocked" and "danger"
            or "accent"
        entity.tail_label = current_step and ("Working: " .. current_step.name)
            or latest.status == "done" and "Pipeline complete"
            or "In pipeline"
        entity.active_work_label = current_step and current_step.name or entity.latest_run_badge
        entity.active_work_detail = latest.status == "done" and "Pipeline steps are complete."
            or latest.status == "blocked" and "Pipeline is blocked."
            or "Pipeline is running."
    else
        entity.latest_run_badge = "ready"
        entity.latest_run_tone = "muted"
        entity.tail_label = "No runs yet"
        entity.active_work_label = "Ready for pipeline"
        entity.active_work_detail = "No run has started for this ticket."
    end

    entity.notification_count = notifications
    entity.notification_label = tostring(notifications) .. " notification"
    entity.secondary_badge = notifications > 0 and (tostring(notifications) .. " notification") or entity.target_label
    entity.secondary_badge_tone = notifications > 0 and "danger" or "muted"
    entity.path = "/pipelines/tickets/" .. entity.id
    return entity
end

local function build_ticket_notification_counts(repo, view)
    if not repo or not view then
        return {}
    end

    local uuid_by_ticket = {}
    local seen_by_ticket = {}
    local all_uuids = {}
    local removed_by_ticket = {}

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

    for _, event in ipairs(rows([[SELECT ticket_id, payload
                                  FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind = 'ticket.manual_session_removed']])) do
        local payload = decode(event.payload, {})
        if not util.is_blank(event.ticket_id) and not util.is_blank(payload.session_uuid) then
            removed_by_ticket[event.ticket_id] = removed_by_ticket[event.ticket_id] or {}
            removed_by_ticket[event.ticket_id][payload.session_uuid] = true
        end
    end

    for _, event in ipairs(rows([[SELECT ticket_id, kind, payload
                                  FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind IN ('ticket.merge_requested',
                                                 'ticket.merge_agent_linked',
                                                 'ticket.manual_session_linked',
                                                 'question.agent_linked')]])) do
        local payload = decode(event.payload, {})
        if not (removed_by_ticket[event.ticket_id] and removed_by_ticket[event.ticket_id][payload.session_uuid]) then
            add(event.ticket_id, payload.session_uuid)
        end
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
    local dependencies = rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                                FROM ticket_dependencies td
                                LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                                ORDER BY td.created_at ASC, td.id ASC]])
    local dependency_levels, dependencies_by_ticket = ticket_dependency_levels(tickets, dependencies)
    local merge_events = latest_merge_events_by_ticket()
    local merge_artifacts = merge_artifacts_by_run()
    local notification_counts = build_ticket_notification_counts(repo, view)
    local out = {}
    for _, ticket in ipairs(tickets or {}) do
        out[#out + 1] = decorate_ticket_entity(copy(ticket), {
            view = view,
            runs = runs_by_ticket[ticket.id] or {},
            steps_by_id = steps_by_id,
            dependencies = dependencies_by_ticket[ticket.id] or {},
            dependency_level = dependency_levels[ticket.id] or 0,
            merge_event = merge_events[ticket.id],
            merge_artifacts_by_run = merge_artifacts,
            notifications = notification_counts[ticket.id] or 0,
        })
    end
    table.sort(out, function(a, b)
        if tostring(a.project_id or "") == tostring(b.project_id or "") then
            return tostring(a.project_sort_key or "") < tostring(b.project_sort_key or "")
        end
        return tostring(a.project_id or "") < tostring(b.project_id or "")
    end)
    return out
end

local function ticket_entity(ticket)
    local entity = copy(ticket)
    local repo = with_repo()
    local view = with_view()
    local runs = repo and repo.ticket_runs(ticket.id) or {}
    local steps_by_id = index_by_id(rows("SELECT * FROM pipeline_steps"))
    local dependencies = rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                                FROM ticket_dependencies td
                                LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                                WHERE td.ticket_id = ?
                                ORDER BY td.created_at ASC, td.id ASC]], ticket.id)
    local project_tickets = {}
    local project_dependencies = {}
    if util.is_blank(ticket.project_id) then
        project_tickets = { ticket }
        project_dependencies = dependencies
    else
        project_tickets = rows("SELECT * FROM tickets WHERE project_id = ?", ticket.project_id)
        project_dependencies = rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                                      FROM ticket_dependencies td
                                      LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                                      JOIN tickets owner ON owner.id = td.ticket_id
                                      WHERE owner.project_id = ?
                                      ORDER BY td.created_at ASC, td.id ASC]], ticket.project_id)
    end
    local dependency_levels = ticket_dependency_levels(project_tickets, project_dependencies)
    local notifications = view and repo and view.ticket_notification_count(ticket.id, repo) or 0
    local merge_event = rows([[SELECT * FROM events
                               WHERE ticket_id = ?
                                 AND kind IN ('ticket.merge_requested', 'ticket.merge_agent_linked')
                               ORDER BY created_at DESC, id DESC
                               LIMIT 1]], ticket.id)[1]
    return decorate_ticket_entity(entity, {
        view = view,
        runs = runs,
        steps_by_id = steps_by_id,
        dependencies = dependencies,
        dependency_level = dependency_levels[ticket.id] or 0,
        merge_event = merge_event,
        merge_artifacts_by_run = merge_artifacts_by_run(),
        notifications = notifications,
    })
end

local function project_entity(project)
    local entity = copy(project)
    local view = with_view()
    entity.status_tone = view and view.status_tone(project.status) or "muted"
    entity.status_label = view and view.status_label(project.status) or tostring(project.status or "")
    entity.status_state = view and view.status_state(project.status) or "neutral"
    entity.path = "/pipelines/projects/" .. project.id
    return entity
end

local function project_target_entity(target)
    local entity = copy(target)
    local view = with_view()
    entity.target_label = target_label(view, target.target_id)
    return entity
end

local function pipeline_step_summary(steps)
    local labels = {}
    for _, step in ipairs(steps or {}) do
        local bits = {
            "#" .. tostring(step.position or ""),
            tostring(step.name or step.id or ""),
            "(" .. tostring(step.kind or "") .. ")",
        }
        if step.agent_name and step.agent_name ~= "" then
            bits[#bits + 1] = step.agent_name
        end
        if step.command and step.command ~= "" then
            bits[#bits + 1] = step.command
        end
        labels[#labels + 1] = table.concat(bits, " ")
    end
    if #labels == 0 then
        return "No steps configured."
    end
    return table.concat(labels, "\n")
end

local function pipeline_entity(pipeline)
    local entity = copy(pipeline)
    local steps = rows("SELECT * FROM pipeline_steps WHERE pipeline_id = ? ORDER BY position ASC, created_at ASC, id ASC", pipeline.id)
    local step_count = #steps
    entity.step_count = step_count
    entity.step_count_label = string.format("%d step%s", step_count, step_count == 1 and "" or "s")
    entity.step_summary = pipeline_step_summary(steps)
    entity.edit_path = "/pipelines/pipelines/" .. pipeline.id .. "/edit"
    return entity
end

local function pipeline_entities(pipelines)
    local counts = {}
    local steps_by_pipeline = {}
    for _, row in ipairs(rows("SELECT pipeline_id, COUNT(*) AS count FROM pipeline_steps GROUP BY pipeline_id")) do
        counts[row.pipeline_id] = tonumber(row.count or 0) or 0
    end
    for _, step in ipairs(rows("SELECT * FROM pipeline_steps ORDER BY pipeline_id ASC, position ASC, created_at ASC, id ASC")) do
        steps_by_pipeline[step.pipeline_id] = steps_by_pipeline[step.pipeline_id] or {}
        table.insert(steps_by_pipeline[step.pipeline_id], step)
    end
    local out = {}
    for _, pipeline in ipairs(pipelines or {}) do
        local entity = copy(pipeline)
        local step_count = counts[pipeline.id] or 0
        entity.step_count = step_count
        entity.step_count_label = string.format("%d step%s", step_count, step_count == 1 and "" or "s")
        entity.step_summary = pipeline_step_summary(steps_by_pipeline[pipeline.id])
        entity.edit_path = "/pipelines/pipelines/" .. pipeline.id .. "/edit"
        out[#out + 1] = entity
    end
    return out
end

local function run_entities(runs)
    local tickets_by_id = index_by_id(rows("SELECT id, title FROM tickets"))
    local pipelines_by_id = index_by_id(rows("SELECT id, name FROM pipelines"))
    local steps_by_id = index_by_id(rows("SELECT id, name FROM pipeline_steps"))
    local view = with_view()
    local out = {}
    for _, run in ipairs(runs or {}) do
        local entity = copy(run)
        local ticket = tickets_by_id[run.ticket_id]
        local pipeline = pipelines_by_id[run.pipeline_id]
        local step = run.current_step_id and steps_by_id[run.current_step_id] or nil
        entity.ticket_title = ticket and ticket.title or run.ticket_id
        entity.pipeline_name = pipeline and pipeline.name or run.pipeline_id
        entity.current_step_name = step and step.name or "No current step"
        entity.status_tone = view and view.status_tone(run.status) or "muted"
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
    local step = run.current_step_id and rows("SELECT * FROM pipeline_steps WHERE id = ? LIMIT 1", run.current_step_id)[1] or nil
    local view = with_view()
    entity.ticket_title = ticket and ticket.title or run.ticket_id
    entity.pipeline_name = pipeline and pipeline.name or run.pipeline_id
    entity.current_step_name = step and step.name or "No current step"
    entity.status_tone = view and view.status_tone(run.status) or "muted"
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
    entity.blocking_label = question.blocking == 1 and "blocking" or "question"
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

local function checklist_item_entity(item)
    local entity = copy(item)
    entity.evidence = decode(item.evidence, {})
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
        all = function()
            local out = {}
            for _, target in ipairs(rows("SELECT * FROM project_targets ORDER BY created_at ASC, id ASC")) do
                out[#out + 1] = project_target_entity(target)
            end
            return out
        end,
        one = project_target_entity,
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
    [M.types.checklist] = {
        all = function()
            return rows("SELECT * FROM checklists ORDER BY updated_at DESC, created_at DESC, id DESC")
        end,
        one = copy,
    },
    [M.types.checklist_item] = {
        all = function()
            local out = {}
            for _, item in ipairs(rows("SELECT * FROM checklist_items ORDER BY checklist_id ASC, position ASC, created_at ASC, id ASC")) do
                out[#out + 1] = checklist_item_entity(item)
            end
            return out
        end,
        one = checklist_item_entity,
    },
    [M.types.pr_link] = {
        all = function()
            return rows("SELECT * FROM pr_links ORDER BY updated_at DESC, created_at DESC, id DESC")
        end,
        one = copy,
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
