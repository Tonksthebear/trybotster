-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/repo.lua
-- @scope device
-- @version 1.1.0

local db = require("project_pipelines.db")
local util = require("project_pipelines.util")

local M = {}
local PERF = os.getenv("BOTSTER_LUA_PERF") == "1"

local function elapsed_ms(started)
    return math.floor(((os.clock() - started) * 1000) + 0.5)
end

local function sql_label(sql)
    local label = tostring(sql or ""):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
    if #label > 120 then
        label = label:sub(1, 117) .. "..."
    end
    return label
end

local function log_perf(message)
    if PERF and log and log.info then
        log.info("[PERF][project_pipelines.repo] " .. message)
    end
end

local function removed_manual_session_set(ticket_id)
    local removed = {}
    if util.is_blank(ticket_id) then
        return removed
    end
    for _, event in ipairs(M.ticket_events(ticket_id, "ticket.manual_session_removed", 200)) do
        local payload = util.decode(event.payload, {})
        if not util.is_blank(payload.session_uuid) then
            removed[payload.session_uuid] = true
        end
    end
    return removed
end

local PIPELINE_UPDATE_FIELDS = {
    name = true,
    description = true,
    merge_policy = true,
    version_label = true,
    archived = true,
    archived_at = true,
    replacement_pipeline_id = true,
    supersedes_pipeline_id = true,
}

local STEP_UPDATE_FIELDS = {
    name = true,
    position = true,
    kind = true,
    agent_name = true,
    prompt = true,
    command = true,
    next_step_id = true,
    on_approved_step_id = true,
    on_changes_requested_step_id = true,
    on_blocked_step_id = true,
}

local GATE_UPDATE_FIELDS = {
    kind = true,
    prompt = true,
    required_fields = true,
    command = true,
}

local TICKET_UPDATE_FIELDS = {
    title = true,
    description = true,
    project_id = true,
    target_id = true,
    status = true,
}

local PROJECT_UPDATE_FIELDS = {
    name = true,
    description = true,
    status = true,
}

local CHECKLIST_UPDATE_FIELDS = {
    name = true,
    description = true,
    source = true,
}

local CHECKLIST_ITEM_UPDATE_FIELDS = {
    position = true,
    prompt = true,
    status = true,
    source_ref = true,
    evidence = true,
}

--- Resolve a spawn target's filesystem root from its target_id.
--
-- target_path is never stored on tickets or runs; it is derived here from the
-- device-scoped spawn target registry whenever a real path is needed (command
-- step cwd, repo-config scanning). Returns nil when the target is unknown or
-- the registry primitive is unavailable.
-- @param target_id string Spawn target identifier
-- @return string|nil Canonical filesystem root, or nil
function M.resolve_target_path(target_id)
    if util.is_blank(target_id) then
        return nil
    end
    if not (spawn_targets and spawn_targets.get) then
        return nil
    end
    local ok, target = pcall(spawn_targets.get, target_id)
    if ok and target and not util.is_blank(target.path) then
        return target.path
    end
    return nil
end

local PR_LINK_UPDATE_FIELDS = {
    provider = true,
    repo = true,
    pr_number = true,
    pr_url = true,
    ticket_id = true,
    run_id = true,
    status = true,
    head_branch = true,
    base_branch = true,
    merge_commit = true,
    updated_at = true,
    merged_at = true,
}

local function rows(sql, ...)
    local params = { ... }
    local result
    local started = PERF and os.clock() or nil
    if #params == 0 then
        result = db:eval(sql)
    elseif #params == 1 then
        result = db:eval(sql, params[1])
    else
        result = db:eval(sql, params)
    end
    if started then
        local count = type(result) == "table" and #result or 0
        local elapsed = elapsed_ms(started)
        log_perf(string.format(
            "rows=%d params=%d elapsed_ms=%d sql=%s",
            count,
            #params,
            elapsed,
            sql_label(sql)))
    end
    if type(result) == "table" then
        return result
    end
    return {}
end

local function first(sql, ...)
    return util.first(rows(sql, ...))
end

local function placeholders(count)
    local parts = {}
    for index = 1, count do
        parts[index] = "?"
    end
    return table.concat(parts, ",")
end

local function id_filter(field, ids)
    if type(ids) ~= "table" or #ids == 0 then
        return "", {}
    end
    return " AND " .. field .. " IN (" .. placeholders(#ids) .. ")", ids
end

local function group_by(items, key)
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

local function index_by_id(items)
    local out = {}
    for _, item in ipairs(items or {}) do
        if item.id then
            out[item.id] = item
        end
    end
    return out
end

local function event_exists(kind)
    local result = first("SELECT COUNT(*) AS count FROM events WHERE kind = ?", kind)
    return result and tonumber(result.count or 0) > 0
end

local function with_transaction(fn)
    return db:execute(function()
        return fn()
    end)
end

local function normalize_merge_policy(value)
    if util.is_blank(value) then
        return "direct"
    end
    if value ~= "direct" and value ~= "pr" then
        error("merge_policy must be direct or pr")
    end
    return value
end

local function publish_entity(entity_key, row)
    local ok, entities = pcall(require, "project_pipelines.entities")
    if ok and entities and entities.types and entities.types[entity_key] and type(entities.upsert) == "function" then
        pcall(entities.upsert, entities.types[entity_key], row)
    end
end

local function publish_entity_snapshot(entity_key)
    local ok, entities = pcall(require, "project_pipelines.entities")
    if ok and entities and entities.types and entities.types[entity_key] and type(entities.snapshot) == "function" then
        pcall(entities.snapshot, entities.types[entity_key])
    end
end

local function remove_entity(entity_key, id)
    local ok, entities = pcall(require, "project_pipelines.entities")
    if ok and entities and entities.types and entities.types[entity_key] and type(entities.remove) == "function" then
        pcall(entities.remove, entities.types[entity_key], id)
    end
end

local function publish_ticket_project_family(ticket_id, depends_on_ticket_id)
    local ticket = M.get_ticket(ticket_id)
    if not ticket then
        return
    end

    if util.is_blank(ticket.project_id) then
        publish_entity("ticket", ticket)
        if not util.is_blank(depends_on_ticket_id) and depends_on_ticket_id ~= ticket_id then
            publish_entity("ticket", M.get_ticket(depends_on_ticket_id))
        end
        return
    end

    for _, related in ipairs(M.project_tickets(ticket.project_id)) do
        publish_entity("ticket", related)
    end
end

local TICKET_DERIVED_EVENT_KINDS = {
    ["ticket.merge_requested"] = true,
    ["ticket.merge_agent_linked"] = true,
    ["question.agent_linked"] = true,
}

local function filter_update(attrs, allowed)
    local set = {}
    for key, value in pairs(attrs or {}) do
        if not allowed[key] then
            error("field cannot be updated: " .. tostring(key))
        end
        set[key] = value
    end
    return set
end

local function has_fields(attrs)
    for _key, _value in pairs(attrs or {}) do
        return true
    end
    return false
end

local function merge(row, attrs)
    local out = util.copy(row or {})
    for key, value in pairs(attrs or {}) do
        out[key] = value
    end
    return out
end

local function dependency_reaches(start_ticket_id, target_ticket_id, seen)
    if util.is_blank(start_ticket_id) or util.is_blank(target_ticket_id) then
        return false
    end
    if start_ticket_id == target_ticket_id then
        return true
    end
    seen = seen or {}
    if seen[start_ticket_id] then
        return false
    end
    seen[start_ticket_id] = true
    for _, dependency in ipairs(rows("SELECT depends_on_ticket_id FROM ticket_dependencies WHERE ticket_id = ?", start_ticket_id)) do
        if dependency_reaches(dependency.depends_on_ticket_id, target_ticket_id, seen) then
            return true
        end
    end
    return false
end

local function decode_required_fields(value)
    if type(value) == "string" then
        return util.decode(value, {})
    end
    return value or {}
end

local function encode_required_fields(value)
    if type(value) == "string" then
        return value
    end
    return util.encode(value or {})
end

local function assert_gate_valid(attrs)
    local kind = attrs.kind or "attestation"
    if kind == "command" then
        util.assert_present(attrs.command, "gate command")
    elseif kind == "attestation" then
        local required_fields = decode_required_fields(attrs.required_fields)
        if #required_fields == 0 then
            error("attestation gates require at least one required field")
        end
    end
end

local function assert_next_step_in_pipeline(next_step_id, pipeline_id)
    if util.is_blank(next_step_id) then
        return
    end
    local next_step = db.pipeline_steps:where{ id = next_step_id }
    if not next_step then
        error("next_step_id not found: " .. tostring(next_step_id))
    end
    if next_step.pipeline_id ~= pipeline_id then
        error("next_step_id must belong to the same pipeline")
    end
end

local function assert_step_links_in_pipeline(attrs, pipeline_id)
    assert_next_step_in_pipeline(attrs.next_step_id, pipeline_id)
    assert_next_step_in_pipeline(attrs.on_approved_step_id, pipeline_id)
    assert_next_step_in_pipeline(attrs.on_changes_requested_step_id, pipeline_id)
    assert_next_step_in_pipeline(attrs.on_blocked_step_id, pipeline_id)
end

local function decode_gate_row(gate)
    local decoded = util.copy(gate or {})
    decoded.required_fields = decode_required_fields(decoded.required_fields)
    return decoded
end

local function assert_checklist_scope(scope)
    if scope ~= "project" and scope ~= "ticket" and scope ~= "run" then
        error("checklist scope must be project, ticket, or run")
    end
end

local function normalize_checklist_status(status)
    status = status or "pending"
    if status ~= "pending" and status ~= "in_progress" and status ~= "blocked" and status ~= "skipped" and status ~= "done" then
        error("checklist item status must be pending, in_progress, blocked, skipped, or done")
    end
    return status
end

local function normalize_pr_provider(provider)
    if util.is_blank(provider) then
        return "github"
    end
    return tostring(provider)
end

local function normalize_pr_status(status)
    status = status or "open"
    if status ~= "open" and status ~= "closed" and status ~= "merged" then
        error("PR link status must be open, closed, or merged")
    end
    return status
end

local function encode_evidence(value)
    if type(value) == "string" then
        return value
    end
    return util.encode(value or {})
end

local function decode_checklist_item(row)
    local decoded = util.copy(row or {})
    decoded.evidence = util.decode(decoded.evidence, {})
    return decoded
end

local function decode_checklist_items(items)
    local out = {}
    for _, item in ipairs(items or {}) do
        out[#out + 1] = decode_checklist_item(item)
    end
    return out
end

local function insert_gate(attrs, now)
    util.assert_present(attrs.step_id, "step_id")
    util.assert_present(attrs.prompt, "gate prompt")
    assert_gate_valid(attrs)
    local gate = {
        id = attrs.id or util.id("gate"),
        step_id = attrs.step_id,
        kind = attrs.kind or "attestation",
        prompt = attrs.prompt,
        required_fields = encode_required_fields(attrs.required_fields),
        command = attrs.command,
        created_at = now or util.now(),
        updated_at = now or util.now(),
    }
    db.pipeline_gates:insert(gate)
    return gate
end

local function insert_step(attrs, now)
    util.assert_present(attrs.pipeline_id, "pipeline_id")
    util.assert_present(attrs.name, "step name")
    assert_step_links_in_pipeline(attrs, attrs.pipeline_id)
    local step = {
        id = attrs.id or util.id("step"),
        pipeline_id = attrs.pipeline_id,
        position = attrs.position,
        kind = attrs.kind or "agent",
        name = attrs.name,
        agent_name = attrs.agent_name,
        prompt = attrs.prompt or "",
        command = attrs.command,
        next_step_id = attrs.next_step_id,
        on_approved_step_id = attrs.on_approved_step_id,
        on_changes_requested_step_id = attrs.on_changes_requested_step_id,
        on_blocked_step_id = attrs.on_blocked_step_id,
        created_at = now or util.now(),
        updated_at = now or util.now(),
    }
    db.pipeline_steps:insert(step)
    return step
end

function M.append_event(kind, attrs)
    attrs = attrs or {}
    local event = {
        id = util.id("event"),
        run_id = attrs.run_id,
        ticket_id = attrs.ticket_id,
        kind = kind,
        payload = util.encode(attrs.payload or {}),
        created_at = util.now(),
    }
    db.events:insert(event)
    publish_entity("event", event)
    if TICKET_DERIVED_EVENT_KINDS[event.kind] and not util.is_blank(event.ticket_id) then
        publish_entity("ticket", M.get_ticket(event.ticket_id))
    end
    return event
end

function M.list_tickets()
    return rows("SELECT * FROM tickets ORDER BY updated_at DESC, created_at DESC")
end

function M.visible_tickets()
    return rows("SELECT * FROM tickets WHERE COALESCE(status, 'open') != 'closed' ORDER BY updated_at DESC, created_at DESC")
end

function M.standalone_tickets()
    return rows([[SELECT * FROM tickets
                  WHERE (project_id IS NULL OR project_id = '')
                  ORDER BY updated_at DESC, created_at DESC]])
end

function M.search_tickets(filters)
    filters = filters or {}
    local where = {}
    local params = {}
    if not filters.include_closed then
        table.insert(where, "COALESCE(status, 'open') != 'closed'")
    end
    if not util.is_blank(filters.status) then
        table.insert(where, "status = ?")
        table.insert(params, filters.status)
    end
    if not util.is_blank(filters.project_id) then
        table.insert(where, "project_id = ?")
        table.insert(params, filters.project_id)
    end
    if not util.is_blank(filters.target_id) then
        table.insert(where, "target_id = ?")
        table.insert(params, filters.target_id)
    end
    if not util.is_blank(filters.query) then
        table.insert(where, "(lower(title) LIKE ? OR lower(description) LIKE ? OR lower(id) LIKE ?)")
        local pattern = "%" .. tostring(filters.query):lower() .. "%"
        table.insert(params, pattern)
        table.insert(params, pattern)
        table.insert(params, pattern)
    end

    local sql = "SELECT * FROM tickets"
    if #where > 0 then
        sql = sql .. " WHERE " .. table.concat(where, " AND ")
    end
    sql = sql .. " ORDER BY updated_at DESC, created_at DESC LIMIT ?"
    table.insert(params, tonumber(filters.limit) or 25)
    return rows(sql, table.unpack(params))
end

function M.project_tickets(project_id)
    return rows("SELECT * FROM tickets WHERE project_id = ? ORDER BY updated_at DESC, created_at DESC", project_id)
end

function M.project_dependency_overview(project_id)
    local tickets = M.project_tickets(project_id)
    local dependencies_by_ticket = {}
    for _, dependency in ipairs(rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                                      FROM ticket_dependencies td
                                      JOIN tickets owner ON owner.id = td.ticket_id
                                      LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                                      WHERE owner.project_id = ?
                                      ORDER BY td.created_at ASC, td.id ASC]], project_id)) do
        local key = dependency.ticket_id
        dependencies_by_ticket[key] = dependencies_by_ticket[key] or {}
        table.insert(dependencies_by_ticket[key], dependency)
    end

    local latest_run_by_ticket = {}
    local open_run_by_ticket = {}
    for _, run in ipairs(rows([[SELECT r.*
                                FROM runs r
                                JOIN tickets t ON t.id = r.ticket_id
                                WHERE t.project_id = ?
                                ORDER BY r.ticket_id ASC, r.created_at DESC, r.id DESC]], project_id)) do
        if not latest_run_by_ticket[run.ticket_id] then
            latest_run_by_ticket[run.ticket_id] = run
        end
        if not open_run_by_ticket[run.ticket_id]
            and (run.status == "active" or run.status == "blocked")
        then
            open_run_by_ticket[run.ticket_id] = run
        end
    end

    return {
        tickets = tickets,
        dependencies_by_ticket = dependencies_by_ticket,
        latest_run_by_ticket = latest_run_by_ticket,
        open_run_by_ticket = open_run_by_ticket,
    }
end

function M.visible_project_tickets(project_id)
    return rows("SELECT * FROM tickets WHERE project_id = ? AND COALESCE(status, 'open') != 'closed' ORDER BY updated_at DESC, created_at DESC", project_id)
end

function M.get_ticket(ticket_id)
    return db.tickets:where{ id = ticket_id }
end

function M.ticket_status(ticket_id)
    local ticket = M.get_ticket(ticket_id)
    if not ticket then
        return nil
    end
    local runs = M.ticket_runs(ticket_id)
    local latest_run = M.latest_ticket_run(ticket_id)
    return {
        ticket = ticket,
        project = ticket.project_id and M.get_project(ticket.project_id) or nil,
        runs = runs,
        latest_run = latest_run,
        latest_run_steps = latest_run and M.run_steps(latest_run.id) or {},
        sessions = M.ticket_session_uuids(ticket_id),
        dependencies = M.ticket_dependencies(ticket_id),
        blocking_dependencies = M.blocking_ticket_dependencies(ticket_id),
        checklists = M.list_checklists{ scope = "ticket", owner_id = ticket_id },
        open_findings = latest_run and M.open_findings(latest_run.id) or {},
        open_questions = M.ticket_questions(ticket_id, "open"),
    }
end

function M.get_pr_link(link_id)
    return db.pr_links:where{ id = link_id }
end

function M.find_pr_link(attrs)
    attrs = attrs or {}
    local provider = normalize_pr_provider(attrs.provider)
    util.assert_present(attrs.repo, "repo")
    util.assert_present(attrs.pr_number, "pr_number")
    return first(
        "SELECT * FROM pr_links WHERE provider = ? AND repo = ? AND pr_number = ? LIMIT 1",
        provider,
        attrs.repo,
        tonumber(attrs.pr_number)
    )
end

function M.list_pr_links(filters)
    filters = filters or {}
    local clauses = {}
    local params = {}
    if not util.is_blank(filters.ticket_id) then
        clauses[#clauses + 1] = "ticket_id = ?"
        params[#params + 1] = filters.ticket_id
    end
    if not util.is_blank(filters.run_id) then
        clauses[#clauses + 1] = "run_id = ?"
        params[#params + 1] = filters.run_id
    end
    if not util.is_blank(filters.status) then
        clauses[#clauses + 1] = "status = ?"
        params[#params + 1] = filters.status
    end
    if not util.is_blank(filters.provider) then
        clauses[#clauses + 1] = "provider = ?"
        params[#params + 1] = normalize_pr_provider(filters.provider)
    end
    if not util.is_blank(filters.repo) then
        clauses[#clauses + 1] = "repo = ?"
        params[#params + 1] = filters.repo
    end

    local sql = "SELECT * FROM pr_links"
    if #clauses > 0 then
        sql = sql .. " WHERE " .. table.concat(clauses, " AND ")
    end
    sql = sql .. " ORDER BY updated_at DESC, created_at DESC, id DESC"
    if #params == 0 then
        return rows(sql)
    end
    return rows(sql, table.unpack(params))
end

function M.link_pr(attrs)
    attrs = attrs or {}
    util.assert_present(attrs.ticket_id, "ticket_id")
    util.assert_present(attrs.repo, "repo")
    util.assert_present(attrs.pr_number, "pr_number")
    if not M.get_ticket(attrs.ticket_id) then
        error("ticket not found: " .. tostring(attrs.ticket_id))
    end
    if not util.is_blank(attrs.run_id) and not M.get_run(attrs.run_id) then
        error("run not found: " .. tostring(attrs.run_id))
    end

    local now = util.now()
    local provider = normalize_pr_provider(attrs.provider)
    local existing = M.find_pr_link{
        provider = provider,
        repo = attrs.repo,
        pr_number = attrs.pr_number,
    }
    local set = {
        provider = provider,
        repo = attrs.repo,
        pr_number = tonumber(attrs.pr_number),
        pr_url = attrs.pr_url,
        ticket_id = attrs.ticket_id,
        run_id = attrs.run_id,
        status = normalize_pr_status(attrs.status),
        head_branch = attrs.head_branch,
        base_branch = attrs.base_branch,
        merge_commit = attrs.merge_commit,
        merged_at = attrs.merged_at,
        updated_at = now,
    }

    local link
    if existing then
        db.pr_links:update{
            where = { id = existing.id },
            set = filter_update(set, PR_LINK_UPDATE_FIELDS),
        }
        link = M.get_pr_link(existing.id)
    else
        link = merge(set, {
            id = attrs.id or util.id("pr"),
            created_at = now,
        })
        db.pr_links:insert(link)
    end

    publish_entity("pr_link", link)
    publish_entity("ticket", M.get_ticket(link.ticket_id))
    M.append_event("ticket.pr_linked", {
        ticket_id = link.ticket_id,
        run_id = link.run_id,
        payload = {
            pr_link_id = link.id,
            provider = link.provider,
            repo = link.repo,
            pr_number = link.pr_number,
            pr_url = link.pr_url,
        },
    })
    return link
end

function M.mark_pr_link_merged(link_id, attrs)
    attrs = attrs or {}
    local link = M.get_pr_link(link_id)
    if not link then
        error("PR link not found: " .. tostring(link_id))
    end
    local now = util.now()
    db.pr_links:update{
        where = { id = link_id },
        set = {
            status = "merged",
            pr_url = attrs.pr_url or link.pr_url,
            head_branch = attrs.head_branch or link.head_branch,
            base_branch = attrs.base_branch or link.base_branch,
            merge_commit = attrs.merge_commit or link.merge_commit,
            merged_at = attrs.merged_at or now,
            updated_at = now,
        },
    }
    link = M.get_pr_link(link_id)
    publish_entity("pr_link", link)
    publish_entity("ticket", M.get_ticket(link.ticket_id))
    M.append_event("ticket.pr_merged", {
        ticket_id = link.ticket_id,
        run_id = link.run_id,
        payload = {
            pr_link_id = link.id,
            provider = link.provider,
            repo = link.repo,
            pr_number = link.pr_number,
            pr_url = link.pr_url,
            merge_commit = link.merge_commit,
        },
    })
    return link
end

function M.list_checklists(filters)
    filters = filters or {}
    local clauses = {}
    local params = {}
    if not util.is_blank(filters.scope) then
        clauses[#clauses + 1] = "scope = ?"
        params[#params + 1] = filters.scope
    end
    if not util.is_blank(filters.owner_id) then
        clauses[#clauses + 1] = "owner_id = ?"
        params[#params + 1] = filters.owner_id
    end
    local sql = "SELECT * FROM checklists"
    if #clauses > 0 then
        sql = sql .. " WHERE " .. table.concat(clauses, " AND ")
    end
    sql = sql .. " ORDER BY updated_at DESC, created_at DESC, id DESC"
    if #params == 0 then
        return rows(sql)
    end
    return rows(sql, params)
end

function M.checklist_items(checklist_id)
    return decode_checklist_items(rows("SELECT * FROM checklist_items WHERE checklist_id = ? ORDER BY position ASC, created_at ASC, id ASC", checklist_id))
end

function M.get_checklist(checklist_id)
    local checklist = db.checklists:where{ id = checklist_id }
    if not checklist then
        return nil
    end
    checklist.items = M.checklist_items(checklist_id)
    return checklist
end

function M.create_checklist(attrs)
    assert_checklist_scope(attrs.scope)
    util.assert_present(attrs.owner_id, "owner_id")
    util.assert_present(attrs.name, "checklist name")
    local now = util.now()
    local checklist = {
        id = attrs.id or util.id("checklist"),
        scope = attrs.scope,
        owner_id = attrs.owner_id,
        name = attrs.name,
        description = attrs.description or "",
        source = attrs.source or "manual",
        created_at = now,
        updated_at = now,
    }
    with_transaction(function()
        db.checklists:insert(checklist)
        for index, item in ipairs(attrs.items or {}) do
            util.assert_present(item.prompt, "checklist item prompt")
            db.checklist_items:insert{
                id = item.id or util.id("check"),
                checklist_id = checklist.id,
                position = item.position or index,
                prompt = item.prompt,
                status = normalize_checklist_status(item.status),
                source_ref = item.source_ref,
                evidence = encode_evidence(item.evidence),
                created_at = now,
                updated_at = now,
                completed_at = item.status == "done" and now or nil,
            }
        end
        M.append_event("checklist.created", {
            ticket_id = attrs.scope == "ticket" and attrs.owner_id or nil,
            run_id = attrs.scope == "run" and attrs.owner_id or nil,
            payload = { checklist_id = checklist.id, scope = checklist.scope, owner_id = checklist.owner_id, source = checklist.source },
        })
    end)
    publish_entity("checklist", checklist)
    publish_entity_snapshot("checklist_item")
    return M.get_checklist(checklist.id)
end

function M.create_vault_checklist(attrs)
    local scope = attrs.scope or "ticket"
    assert_checklist_scope(scope)
    util.assert_present(attrs.owner_id, "owner_id")
    local items = {
        {
            prompt = "Load applicable vault/project conventions before planning.",
            source_ref = "vault:context",
        },
        {
            prompt = "Check the implementation plan against the loaded conventions and record conflicts or 'none'.",
            source_ref = "vault:plan-review",
        },
        {
            prompt = "Verify with repo-approved commands and attach command evidence.",
            source_ref = "vault:verification",
        },
        {
            prompt = "Capture new durable project knowledge in the vault, or record why no capture was needed.",
            source_ref = "vault:capture",
        },
    }
    return M.create_checklist{
        scope = scope,
        owner_id = attrs.owner_id,
        name = attrs.name or "Vault workflow checklist",
        description = attrs.description or "Tracks whether the agent used the vault as the source of truth without duplicating conventions into Project Pipelines.",
        source = "vault",
        items = attrs.items or items,
    }
end

function M.update_checklist(checklist_id, attrs)
    local checklist = db.checklists:where{ id = checklist_id }
    if not checklist then
        error("checklist not found: " .. tostring(checklist_id))
    end
    local fields = filter_update(attrs or {}, CHECKLIST_UPDATE_FIELDS)
    if not has_fields(fields) then
        return M.get_checklist(checklist_id)
    end
    fields.updated_at = util.now()
    db.checklists:update{ where = { id = checklist_id }, set = fields }
    local updated = db.checklists:where{ id = checklist_id }
    publish_entity("checklist", updated)
    M.append_event("checklist.updated", {
        ticket_id = updated.scope == "ticket" and updated.owner_id or nil,
        run_id = updated.scope == "run" and updated.owner_id or nil,
        payload = { checklist_id = checklist_id, fields = fields },
    })
    return M.get_checklist(checklist_id)
end

function M.add_checklist_item(attrs)
    util.assert_present(attrs.checklist_id, "checklist_id")
    util.assert_present(attrs.prompt, "checklist item prompt")
    local checklist = db.checklists:where{ id = attrs.checklist_id }
    if not checklist then
        error("checklist not found: " .. tostring(attrs.checklist_id))
    end
    local now = util.now()
    local item = {
        id = attrs.id or util.id("check"),
        checklist_id = attrs.checklist_id,
        position = attrs.position or (#M.checklist_items(attrs.checklist_id) + 1),
        prompt = attrs.prompt,
        status = normalize_checklist_status(attrs.status),
        source_ref = attrs.source_ref,
        evidence = encode_evidence(attrs.evidence),
        created_at = now,
        updated_at = now,
        completed_at = attrs.status == "done" and now or nil,
    }
    db.checklist_items:insert(item)
    db.checklists:update{ where = { id = attrs.checklist_id }, set = { updated_at = now } }
    publish_entity("checklist_item", decode_checklist_item(item))
    publish_entity("checklist", db.checklists:where{ id = attrs.checklist_id })
    M.append_event("checklist.item_created", {
        ticket_id = checklist.scope == "ticket" and checklist.owner_id or nil,
        run_id = checklist.scope == "run" and checklist.owner_id or nil,
        payload = { checklist_id = attrs.checklist_id, item_id = item.id },
    })
    return decode_checklist_item(item)
end

function M.update_checklist_item(item_id, attrs)
    local item = db.checklist_items:where{ id = item_id }
    if not item then
        error("checklist item not found: " .. tostring(item_id))
    end
    local checklist = db.checklists:where{ id = item.checklist_id }
    local fields = filter_update(attrs or {}, CHECKLIST_ITEM_UPDATE_FIELDS)
    if fields.status ~= nil then
        fields.status = normalize_checklist_status(fields.status)
        fields.completed_at = fields.status == "done" and util.now() or nil
    end
    if fields.evidence ~= nil then
        fields.evidence = encode_evidence(fields.evidence)
    end
    if not has_fields(fields) then
        return decode_checklist_item(item)
    end
    local now = util.now()
    fields.updated_at = now
    db.checklist_items:update{ where = { id = item_id }, set = fields }
    if checklist then
        db.checklists:update{ where = { id = checklist.id }, set = { updated_at = now } }
        publish_entity("checklist", db.checklists:where{ id = checklist.id })
    end
    local updated = decode_checklist_item(db.checklist_items:where{ id = item_id })
    publish_entity("checklist_item", updated)
    M.append_event("checklist.item_updated", {
        ticket_id = checklist and checklist.scope == "ticket" and checklist.owner_id or nil,
        run_id = checklist and checklist.scope == "run" and checklist.owner_id or nil,
        payload = { checklist_id = item.checklist_id, item_id = item_id, status = updated.status },
    })
    return updated
end

function M.create_ticket(attrs)
    util.assert_present(attrs.title, "title")
    util.assert_present(attrs.target_id, "target_id")
    local now = util.now()
    local ticket = {
        id = attrs.id or util.id("ticket"),
        project_id = attrs.project_id,
        target_id = attrs.target_id,
        title = attrs.title,
        description = attrs.description or "",
        status = attrs.status or "open",
        created_at = now,
        updated_at = now,
    }
    db.tickets:insert(ticket)
    publish_entity("ticket", ticket)
    M.append_event("ticket.created", { ticket_id = ticket.id, payload = ticket })
    return ticket
end

function M.add_ticket_dependency(ticket_id, depends_on_ticket_id)
    util.assert_present(ticket_id, "ticket_id")
    util.assert_present(depends_on_ticket_id, "depends_on_ticket_id")
    if ticket_id == depends_on_ticket_id then
        error("ticket cannot depend on itself")
    end
    if not M.get_ticket(ticket_id) then
        error("ticket not found: " .. tostring(ticket_id))
    end
    if not M.get_ticket(depends_on_ticket_id) then
        error("dependency ticket not found: " .. tostring(depends_on_ticket_id))
    end
    if dependency_reaches(depends_on_ticket_id, ticket_id) then
        error("ticket dependency would create a cycle")
    end
    local existing = rows([[SELECT * FROM ticket_dependencies
                            WHERE ticket_id = ? AND depends_on_ticket_id = ?
                            LIMIT 1]], ticket_id, depends_on_ticket_id)[1]
    if existing then
        return existing
    end
    local dependency = {
        id = util.id("dependency"),
        ticket_id = ticket_id,
        depends_on_ticket_id = depends_on_ticket_id,
        created_at = util.now(),
    }
    db.ticket_dependencies:insert(dependency)
    publish_entity("ticket_dependency", dependency)
    publish_ticket_project_family(ticket_id, depends_on_ticket_id)
    M.append_event("ticket.dependency_added", {
        ticket_id = ticket_id,
        payload = { dependency_id = dependency.id, depends_on_ticket_id = depends_on_ticket_id },
    })
    return dependency
end

function M.remove_ticket_dependency(dependency_id)
    util.assert_present(dependency_id, "dependency_id")
    local dependency = db.ticket_dependencies:where{ id = dependency_id }
    if not dependency then
        error("ticket dependency not found: " .. tostring(dependency_id))
    end
    db:eval("DELETE FROM ticket_dependencies WHERE id = ?", dependency_id)
    remove_entity("ticket_dependency", dependency_id)
    publish_ticket_project_family(dependency.ticket_id, dependency.depends_on_ticket_id)
    M.append_event("ticket.dependency_removed", {
        ticket_id = dependency.ticket_id,
        payload = { dependency_id = dependency.id, depends_on_ticket_id = dependency.depends_on_ticket_id },
    })
    return dependency
end

function M.ticket_dependencies(ticket_id)
    return rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                  FROM ticket_dependencies td
                  LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                  WHERE td.ticket_id = ?
                  ORDER BY td.created_at ASC]], ticket_id)
end

function M.blocking_ticket_dependencies(ticket_id)
    return rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                  FROM ticket_dependencies td
                  LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                  WHERE td.ticket_id = ?
                    AND COALESCE(t.status, 'open') != 'closed'
                  ORDER BY td.created_at ASC]], ticket_id)
end

function M.closed_ticket_dependencies(ticket_id)
    return rows([[SELECT td.*, t.title AS depends_on_title, t.status AS depends_on_status
                  FROM ticket_dependencies td
                  LEFT JOIN tickets t ON t.id = td.depends_on_ticket_id
                  WHERE td.ticket_id = ?
                    AND COALESCE(t.status, 'open') = 'closed'
                  ORDER BY td.created_at ASC]], ticket_id)
end

function M.update_ticket(ticket_id, attrs)
    local ticket = M.get_ticket(ticket_id)
    if not ticket then
        error("ticket not found: " .. tostring(ticket_id))
    end
    local set = filter_update(attrs, TICKET_UPDATE_FIELDS)
    if not has_fields(set) then
        return ticket
    end
    if set.title ~= nil then
        util.assert_present(set.title, "title")
    end
    if set.target_id ~= nil then
        util.assert_present(set.target_id, "target_id")
    end
    set.updated_at = util.now()
    db.tickets:update{ where = { id = ticket_id }, set = set }
    publish_entity("ticket", M.get_ticket(ticket_id))
    M.append_event("ticket.updated", { ticket_id = ticket_id, payload = set })
    return M.get_ticket(ticket_id)
end

function M.delete_ticket(ticket_id)
    local ticket = M.get_ticket(ticket_id)
    if not ticket then
        return nil
    end
    if #M.ticket_runs(ticket_id) > 0 then
        error("cannot delete ticket with run history: " .. tostring(ticket_id))
    end
    local dependencies = rows([[SELECT * FROM ticket_dependencies
                                WHERE ticket_id = ? OR depends_on_ticket_id = ?]], ticket_id, ticket_id)
    with_transaction(function()
        db:eval("DELETE FROM ticket_dependencies WHERE ticket_id = ? OR depends_on_ticket_id = ?", { ticket_id, ticket_id })
        db:eval("DELETE FROM tickets WHERE id = ?", ticket_id)
    end)
    remove_entity("ticket", ticket_id)
    if #dependencies > 0 then
        M.append_event("ticket.dependencies_purged", {
            ticket_id = ticket_id,
            payload = { ticket_id = ticket_id, dependencies = dependencies },
        })
    end
    M.append_event("ticket.deleted", { ticket_id = ticket_id, payload = { ticket_id = ticket_id } })
    return ticket
end

function M.list_projects()
    return rows("SELECT * FROM projects ORDER BY updated_at DESC, created_at DESC")
end

function M.open_projects()
    return rows([[SELECT * FROM projects
                  WHERE COALESCE(status, 'open') != 'closed'
                  ORDER BY updated_at DESC, created_at DESC]])
end

function M.get_project(project_id)
    return db.projects:where{ id = project_id }
end

function M.project_detail(project_id)
    local project = M.get_project(project_id)
    if not project then
        return nil
    end
    return {
        project = project,
        targets = M.project_targets(project_id),
        tickets = M.project_tickets(project_id),
    }
end

function M.create_project(attrs)
    util.assert_present(attrs.name, "name")
    local now = util.now()
    local project = {
        id = attrs.id or util.id("project"),
        name = attrs.name,
        description = attrs.description or "",
        status = attrs.status or "open",
        created_at = now,
        updated_at = now,
    }
    db.projects:insert(project)
    publish_entity("project", project)
    if attrs.target_id then
        M.add_project_target(project.id, attrs.target_id)
    end
    M.append_event("project.created", { payload = project })
    return project
end

function M.update_project(project_id, attrs)
    local project = M.get_project(project_id)
    if not project then
        error("project not found: " .. tostring(project_id))
    end
    local set = filter_update(attrs, PROJECT_UPDATE_FIELDS)
    if not has_fields(set) then
        return project
    end
    if set.name ~= nil then
        util.assert_present(set.name, "name")
    end
    set.updated_at = util.now()
    db.projects:update{ where = { id = project_id }, set = set }
    publish_entity("project", M.get_project(project_id))
    M.append_event("project.updated", { payload = { project_id = project_id, fields = set } })
    return M.get_project(project_id)
end

function M.delete_project(project_id)
    local project = M.get_project(project_id)
    if not project then
        return nil
    end
    if #M.project_tickets(project_id) > 0 then
        error("cannot delete project with tickets: " .. tostring(project_id))
    end
    with_transaction(function()
        db:eval("DELETE FROM project_targets WHERE project_id = ?", project_id)
        db.projects:delete{ id = project_id }
        M.append_event("project.deleted", { payload = { project_id = project_id } })
    end)
    remove_entity("project", project_id)
    return project
end

function M.add_project_target(project_id, target_id)
    util.assert_present(project_id, "project_id")
    util.assert_present(target_id, "target_id")
    if not M.get_project(project_id) then
        error("project not found: " .. tostring(project_id))
    end
    local row = {
        id = util.id("project_target"),
        project_id = project_id,
        target_id = target_id,
        created_at = util.now(),
    }
    db.project_targets:insert(row)
    publish_entity("project_target", row)
    M.append_event("project.target_added", {
        payload = { project_id = project_id, target_id = target_id },
    })
    return row
end

function M.project_targets(project_id)
    return rows("SELECT * FROM project_targets WHERE project_id = ? ORDER BY created_at ASC", project_id)
end

function M.remove_project_target(project_target_id)
    local row = db.project_targets:where{ id = project_target_id }
    if not row then
        return nil
    end
    db.project_targets:delete{ id = project_target_id }
    remove_entity("project_target", project_target_id)
    M.append_event("project.target_removed", {
        payload = { project_id = row.project_id, project_target_id = project_target_id, target_id = row.target_id },
    })
    return row
end

function M.list_pipelines()
    return rows("SELECT * FROM pipelines WHERE archived_at IS NULL ORDER BY created_at ASC")
end

function M.list_all_pipelines()
    return rows("SELECT * FROM pipelines ORDER BY created_at ASC")
end

function M.get_pipeline(pipeline_id)
    return db.pipelines:where{ id = pipeline_id }
end

function M.pipeline_is_archived(pipeline)
    return pipeline and pipeline.archived_at ~= nil and tostring(pipeline.archived_at) ~= ""
end

function M.get_pipeline_definition(pipeline_id)
    local pipeline = M.get_pipeline(pipeline_id)
    if not pipeline then
        return nil
    end
    pipeline.steps = M.pipeline_steps(pipeline.id)
    for _, step in ipairs(pipeline.steps) do
        step.gates = {}
        for _, gate in ipairs(M.step_gates(step.id)) do
            table.insert(step.gates, decode_gate_row(gate))
        end
    end
    return pipeline
end

function M.get_default_pipeline()
    return first("SELECT * FROM pipelines WHERE archived_at IS NULL ORDER BY created_at ASC LIMIT 1")
end

function M.pipeline_steps(pipeline_id)
    return rows("SELECT * FROM pipeline_steps WHERE pipeline_id = ? ORDER BY position ASC, created_at ASC, id ASC", pipeline_id)
end

function M.get_step(step_id)
    return db.pipeline_steps:where{ id = step_id }
end

function M.update_pipeline(pipeline_id, attrs)
    local pipeline = M.get_pipeline(pipeline_id)
    if not pipeline then
        error("pipeline not found: " .. tostring(pipeline_id))
    end
    local set = filter_update(attrs, PIPELINE_UPDATE_FIELDS)
    if not has_fields(set) then
        return pipeline
    end
    local now = util.now()
    if set.merge_policy ~= nil then
        set.merge_policy = normalize_merge_policy(set.merge_policy)
    end
    local clear_columns = {}
    if set.archived ~= nil then
        if set.archived == true or set.archived == 1 or set.archived == "1" or set.archived == "true" then
            set.archived_at = pipeline.archived_at or now
        else
            set.archived_at = nil
            clear_columns[#clear_columns + 1] = "archived_at"
        end
        set.archived = nil
    end
    for _, field in ipairs({ "replacement_pipeline_id", "supersedes_pipeline_id" }) do
        if set[field] ~= nil then
            if util.is_blank(set[field]) then
                set[field] = nil
                clear_columns[#clear_columns + 1] = field
            elseif set[field] == pipeline_id then
                error(field .. " cannot reference the same pipeline")
            elseif not M.get_pipeline(set[field]) then
                error(field .. " not found: " .. tostring(set[field]))
            end
        end
    end
    set.updated_at = now
    db.pipelines:update{ where = { id = pipeline_id }, set = set }
    for _, column in ipairs(clear_columns) do
        db:eval("UPDATE pipelines SET " .. column .. " = NULL WHERE id = ?", pipeline_id)
    end
    publish_entity("pipeline", M.get_pipeline(pipeline_id))
    M.append_event("pipeline.updated", {
        payload = { pipeline_id = pipeline_id, fields = set },
    })
    return M.get_pipeline(pipeline_id)
end

function M.update_step(step_id, attrs)
    local step = M.get_step(step_id)
    if not step then
        error("step not found: " .. tostring(step_id))
    end
    local set = filter_update(attrs, STEP_UPDATE_FIELDS)
    if not has_fields(set) then
        return step
    end
    local proposed = merge(step, set)
    assert_step_links_in_pipeline(proposed, proposed.pipeline_id)
    set.updated_at = util.now()
    db.pipeline_steps:update{ where = { id = step_id }, set = set }
    publish_entity("pipeline_step", M.get_step(step_id))
    publish_entity("pipeline", M.get_pipeline(step.pipeline_id))
    M.append_event("pipeline.step_updated", {
        payload = { step_id = step_id, fields = set },
    })
    return M.get_step(step_id)
end

function M.update_step_agent(step_id, agent_name)
    return M.update_step(step_id, { agent_name = agent_name })
end

function M.get_gate(gate_id)
    return db.pipeline_gates:where{ id = gate_id }
end

function M.update_gate(gate_id, attrs)
    local gate = M.get_gate(gate_id)
    if not gate then
        error("gate not found: " .. tostring(gate_id))
    end
    local set = filter_update(attrs, GATE_UPDATE_FIELDS)
    if not has_fields(set) then
        return decode_gate_row(gate)
    end
    local proposed = merge(gate, set)
    assert_gate_valid(proposed)
    set.updated_at = util.now()
    if set.required_fields and type(set.required_fields) ~= "string" then
        set.required_fields = util.encode(set.required_fields)
    end
    db.pipeline_gates:update{ where = { id = gate_id }, set = set }
    publish_entity("pipeline_gate", decode_gate_row(M.get_gate(gate_id)))
    M.append_event("pipeline.gate_updated", {
        payload = { gate_id = gate_id, fields = set },
    })
    return decode_gate_row(M.get_gate(gate_id))
end

function M.step_gates(step_id)
    return rows("SELECT * FROM pipeline_gates WHERE step_id = ? ORDER BY created_at ASC, id ASC", step_id)
end

function M.create_pipeline(attrs)
    util.assert_present(attrs.id, "pipeline id")
    util.assert_present(attrs.name, "pipeline name")
    local now = util.now()
    local published_step_ids = {}
    local published_gate_ids = {}
    local pipeline = {
        id = attrs.id,
        name = attrs.name,
        description = attrs.description or "",
        merge_policy = normalize_merge_policy(attrs.merge_policy),
        version_label = attrs.version_label,
        archived_at = attrs.archived_at,
        replacement_pipeline_id = attrs.replacement_pipeline_id,
        supersedes_pipeline_id = attrs.supersedes_pipeline_id,
        created_at = now,
        updated_at = now,
    }
    with_transaction(function()
        if M.get_pipeline(pipeline.id) then
            error("pipeline already exists: " .. tostring(pipeline.id))
        end
        for _, field in ipairs({ "replacement_pipeline_id", "supersedes_pipeline_id" }) do
            if not util.is_blank(pipeline[field]) then
                if pipeline[field] == pipeline.id then
                    error(field .. " cannot reference the same pipeline")
                elseif not M.get_pipeline(pipeline[field]) then
                    error(field .. " not found: " .. tostring(pipeline[field]))
                end
            end
        end
        db.pipelines:insert(pipeline)
        local step_ids = {}
        local step_id_lookup = {}
        for index, step in ipairs(attrs.steps or {}) do
            step_ids[index] = step.id or util.id("step")
            step_id_lookup[step_ids[index]] = true
        end
        for index, step in ipairs(attrs.steps or {}) do
            local step_attrs = util.copy(step)
            step_attrs.id = step_ids[index]
            step_attrs.pipeline_id = pipeline.id
            step_attrs.position = step_attrs.position or index
            step_attrs.next_step_id = nil
            step_attrs.on_approved_step_id = nil
            step_attrs.on_changes_requested_step_id = nil
            step_attrs.on_blocked_step_id = nil
            local inserted = insert_step(step_attrs, now)
            published_step_ids[#published_step_ids + 1] = inserted.id
        end
        for index, step in ipairs(attrs.steps or {}) do
            local transition_updates = {}
            for _, field in ipairs({ "next_step_id", "on_approved_step_id", "on_changes_requested_step_id", "on_blocked_step_id" }) do
                if not util.is_blank(step[field]) then
                    if not step_id_lookup[step[field]] then
                        error(field .. " must belong to the same pipeline")
                    end
                    transition_updates[field] = step[field]
                end
            end
            if has_fields(transition_updates) then
                transition_updates.updated_at = now
                db.pipeline_steps:update{
                    where = { id = step_ids[index] },
                    set = transition_updates,
                }
            end
        end
        for index, step in ipairs(attrs.steps or {}) do
            for _, gate in ipairs(step.gates or {}) do
                local gate_attrs = util.copy(gate)
                gate_attrs.step_id = step_ids[index]
                local inserted = insert_gate(gate_attrs, now)
                published_gate_ids[#published_gate_ids + 1] = inserted.id
            end
        end
        M.append_event("pipeline.created", { payload = pipeline })
    end)
    publish_entity("pipeline", M.get_pipeline(pipeline.id))
    for _, step_id in ipairs(published_step_ids) do
        publish_entity("pipeline_step", M.get_step(step_id))
    end
    for _, gate_id in ipairs(published_gate_ids) do
        publish_entity("pipeline_gate", decode_gate_row(M.get_gate(gate_id)))
    end
    return pipeline
end

function M.create_step(attrs)
    util.assert_present(attrs.pipeline_id, "pipeline_id")
    util.assert_present(attrs.name, "step name")
    if not M.get_pipeline(attrs.pipeline_id) then
        error("pipeline not found: " .. tostring(attrs.pipeline_id))
    end
    local now = util.now()
    local step
    local gate_ids = {}
    with_transaction(function()
        local step_attrs = util.copy(attrs)
        step_attrs.position = step_attrs.position or (#M.pipeline_steps(attrs.pipeline_id) + 1)
        step = insert_step(step_attrs, now)
        for _, gate in ipairs(attrs.gates or {}) do
            local gate_attrs = util.copy(gate)
            gate_attrs.step_id = step.id
            local inserted = insert_gate(gate_attrs, now)
            gate_ids[#gate_ids + 1] = inserted.id
        end
        M.append_event("pipeline.step_created", {
            payload = { pipeline_id = step.pipeline_id, step_id = step.id },
        })
    end)
    publish_entity("pipeline_step", M.get_step(step.id))
    publish_entity("pipeline", M.get_pipeline(step.pipeline_id))
    for _, gate_id in ipairs(gate_ids) do
        publish_entity("pipeline_gate", decode_gate_row(M.get_gate(gate_id)))
    end
    return M.get_step(step.id)
end

function M.delete_step(step_id)
    local step = M.get_step(step_id)
    if not step then
        return nil
    end
    local usage = first("SELECT COUNT(*) AS count FROM run_steps WHERE step_id = ?", step_id)
    if usage and tonumber(usage.count or 0) > 0 then
        error("cannot delete pipeline step with existing run history: " .. tostring(step_id))
    end
    local references = first([[SELECT COUNT(*) AS count FROM pipeline_steps
                               WHERE next_step_id = ?
                                  OR on_approved_step_id = ?
                                  OR on_changes_requested_step_id = ?
                                  OR on_blocked_step_id = ?]], step_id, step_id, step_id, step_id)
    if references and tonumber(references.count or 0) > 0 then
        error("cannot delete pipeline step referenced by another step: " .. tostring(step_id))
    end
    local gate_usage = first("SELECT COUNT(*) AS count FROM gate_results WHERE step_id = ?", step_id)
    if gate_usage and tonumber(gate_usage.count or 0) > 0 then
        error("cannot delete pipeline step with existing gate results: " .. tostring(step_id))
    end
    local gate_ids = {}
    for _, gate in ipairs(M.step_gates(step_id)) do
        gate_ids[#gate_ids + 1] = gate.id
    end
    with_transaction(function()
        db:eval("DELETE FROM pipeline_gates WHERE step_id = ?", step_id)
        db:eval("DELETE FROM pipeline_steps WHERE id = ?", step_id)
        M.append_event("pipeline.step_deleted", {
            payload = { pipeline_id = step.pipeline_id, step_id = step_id },
        })
    end)
    for _, gate_id in ipairs(gate_ids) do
        remove_entity("pipeline_gate", gate_id)
    end
    remove_entity("pipeline_step", step_id)
    publish_entity("pipeline", M.get_pipeline(step.pipeline_id))
    return step
end

function M.create_gate(attrs)
    util.assert_present(attrs.step_id, "step_id")
    util.assert_present(attrs.prompt, "gate prompt")
    if not M.get_step(attrs.step_id) then
        error("step not found: " .. tostring(attrs.step_id))
    end
    local gate = insert_gate(attrs, util.now())
    publish_entity("pipeline_gate", decode_gate_row(gate))
    M.append_event("pipeline.gate_created", {
        payload = { step_id = gate.step_id, gate_id = gate.id },
    })
    return decode_gate_row(M.get_gate(gate.id))
end

function M.delete_gate(gate_id)
    local gate = M.get_gate(gate_id)
    if not gate then
        return nil
    end
    local usage = first("SELECT COUNT(*) AS count FROM gate_results WHERE gate_id = ?", gate_id)
    if usage and tonumber(usage.count or 0) > 0 then
        error("cannot delete pipeline gate with existing result history: " .. tostring(gate_id))
    end
    with_transaction(function()
        db:eval("DELETE FROM pipeline_gates WHERE id = ?", gate_id)
        M.append_event("pipeline.gate_deleted", {
            payload = { step_id = gate.step_id, gate_id = gate_id },
        })
    end)
    remove_entity("pipeline_gate", gate_id)
    return gate
end

function M.delete_pipeline(pipeline_id)
    local pipeline = M.get_pipeline(pipeline_id)
    if not pipeline then
        return nil
    end
    local usage = first("SELECT COUNT(*) AS count FROM runs WHERE pipeline_id = ?", pipeline_id)
    if usage and tonumber(usage.count or 0) > 0 then
        error("cannot delete pipeline with existing run history: " .. tostring(pipeline_id))
    end
    local step_ids = {}
    local gate_ids = {}
    for _, step in ipairs(M.pipeline_steps(pipeline_id)) do
        step_ids[#step_ids + 1] = step.id
        for _, gate in ipairs(M.step_gates(step.id)) do
            gate_ids[#gate_ids + 1] = gate.id
        end
    end
    with_transaction(function()
        db:eval("DELETE FROM pipeline_gates WHERE step_id IN (SELECT id FROM pipeline_steps WHERE pipeline_id = ?)", pipeline_id)
        db:eval("DELETE FROM pipeline_steps WHERE pipeline_id = ?", pipeline_id)
        db:eval("DELETE FROM pipelines WHERE id = ?", pipeline_id)
        M.append_event("pipeline.deleted", { payload = { pipeline_id = pipeline_id } })
    end)
    for _, gate_id in ipairs(gate_ids) do
        remove_entity("pipeline_gate", gate_id)
    end
    for _, step_id in ipairs(step_ids) do
        remove_entity("pipeline_step", step_id)
    end
    remove_entity("pipeline", pipeline_id)
    return pipeline
end

function M.prune_legacy_seed_data()
    -- One-time cleanup for pre-CRUD seeded data. Remove after existing hubs have migrated.
    if event_exists("pipeline.legacy_prune_checked") then
        return false
    end
    local changed = false
    if M.get_pipeline("massive_feature") then
        local ok, err = pcall(M.delete_pipeline, "massive_feature")
        if ok then
            M.append_event("pipeline.removed", { payload = { pipeline_id = "massive_feature", reason = "Projects now model multi-phase or cross-target work." } })
            changed = true
        else
            M.append_event("pipeline.legacy_prune_skipped", { payload = { pipeline_id = "massive_feature", error = tostring(err) } })
        end
    end
    M.append_event("pipeline.legacy_prune_checked", { payload = { pipeline_id = "massive_feature", changed = changed } })
    return changed
end

function M.create_run(attrs)
    util.assert_present(attrs.ticket_id, "ticket_id")
    util.assert_present(attrs.pipeline_id, "pipeline_id")
    local now = util.now()
    local run = {
        id = attrs.id or util.id("run"),
        ticket_id = attrs.ticket_id,
        pipeline_id = attrs.pipeline_id,
        status = "active",
        current_step_id = nil,
        current_run_step_id = nil,
        parent_run_id = attrs.parent_run_id,
        target_id = attrs.target_id,
        target_name = attrs.target_name,
        workspace_id = attrs.workspace_id,
        workspace_name = attrs.workspace_name,
        base_ticket_id = attrs.base_ticket_id,
        base_run_id = attrs.base_run_id,
        base_ref = attrs.base_ref,
        base_target_path = attrs.base_target_path,
        created_at = now,
        updated_at = now,
    }
    db.runs:insert(run)
    publish_entity("run", run)
    publish_entity("ticket", M.get_ticket(run.ticket_id))
    M.append_event("run.created", { run_id = run.id, ticket_id = run.ticket_id, payload = run })
    return run
end

function M.get_run(run_id)
    return db.runs:where{ id = run_id }
end

function M.update_run(run_id, attrs)
    local set = util.copy(attrs or {})
    set.updated_at = util.now()
    db.runs:update{ where = { id = run_id }, set = set }
    local run = M.get_run(run_id)
    publish_entity("run", run)
    if run then
        publish_entity("ticket", M.get_ticket(run.ticket_id))
    end
    return run
end

function M.list_runs(limit)
    return rows("SELECT * FROM runs ORDER BY updated_at DESC LIMIT ?", limit or 50)
end

function M.ticket_runs(ticket_id)
    return rows("SELECT * FROM runs WHERE ticket_id = ? ORDER BY created_at DESC", ticket_id)
end

function M.open_ticket_run(ticket_id)
    return first([[SELECT * FROM runs
                   WHERE ticket_id = ? AND status IN ('active', 'blocked')
                   ORDER BY created_at DESC LIMIT 1]], ticket_id)
end

function M.latest_ticket_run(ticket_id)
    return first("SELECT * FROM runs WHERE ticket_id = ? ORDER BY created_at DESC LIMIT 1", ticket_id)
end

function M.latest_merge_pr_artifact(run_id)
    if util.is_blank(run_id) then
        return nil
    end
    for _, artifact in ipairs(rows([[SELECT * FROM artifacts
                                     WHERE run_id = ? AND kind = 'merge'
                                     ORDER BY created_at DESC, id DESC]], run_id)) do
        local payload = util.decode(artifact.payload, {})
        if not util.is_blank(artifact.uri) or not util.is_blank(payload.pr_url) then
            return artifact
        end
    end
    return nil
end

function M.run_steps(run_id)
    return rows([[SELECT rs.*, ps.name, ps.kind, ps.position, ps.agent_name, ps.prompt, ps.command
                  FROM run_steps rs
                  JOIN pipeline_steps ps ON ps.id = rs.step_id
                  WHERE rs.run_id = ?
                  ORDER BY COALESCE(rs.sequence, 0) ASC, rs.created_at ASC, rs.id ASC]], run_id)
end

function M.ticket_run_steps(ticket_id)
    return rows([[SELECT rs.*, r.ticket_id, r.pipeline_id, ps.name, ps.kind, ps.position, ps.agent_name
                  FROM run_steps rs
                  JOIN runs r ON r.id = rs.run_id
                  JOIN pipeline_steps ps ON ps.id = rs.step_id
                  WHERE r.ticket_id = ?
                  ORDER BY r.created_at DESC, COALESCE(rs.sequence, 0) ASC, rs.created_at ASC, rs.id ASC]], ticket_id)
end

function M.run_step_gate_results(run_step_id)
    if util.is_blank(run_step_id) then
        return {}
    end
    return rows("SELECT * FROM gate_results WHERE run_step_id = ? ORDER BY created_at ASC, id ASC", run_step_id)
end

function M.ticket_session_uuids(ticket_id)
    local seen = {}
    local uuids = {}
    local removed_manual_sessions = removed_manual_session_set(ticket_id)
    for _, step in ipairs(M.ticket_run_steps(ticket_id)) do
        if step.agent_session_uuid and step.agent_session_uuid ~= "" and not seen[step.agent_session_uuid] then
            seen[step.agent_session_uuid] = true
            table.insert(uuids, step.agent_session_uuid)
        end
    end
    for _, event in ipairs(M.ticket_events(ticket_id, nil, 100)) do
        if event.kind == "ticket.merge_requested"
            or event.kind == "ticket.merge_agent_linked"
            or event.kind == "ticket.manual_session_linked"
            or event.kind == "question.agent_linked" then
            local payload = util.decode(event.payload, {})
            local uuid = payload.session_uuid
            if event.kind == "ticket.manual_session_linked" and removed_manual_sessions[uuid] then
                uuid = nil
            end
            if uuid and uuid ~= "" and not seen[uuid] then
                seen[uuid] = true
                table.insert(uuids, uuid)
            end
        end
    end
    return uuids
end

function M.ticket_session_uuids_by_ticket(ticket_ids)
    local ticket_filter, params = id_filter("r.ticket_id", ticket_ids)
    local by_ticket = {}
    local seen = {}
    local all_uuids = {}
    local removed_by_ticket = {}

    local function add(ticket_id, uuid)
        if util.is_blank(ticket_id) or util.is_blank(uuid) then
            return
        end
        seen[ticket_id] = seen[ticket_id] or {}
        if seen[ticket_id][uuid] then
            return
        end
        seen[ticket_id][uuid] = true
        by_ticket[ticket_id] = by_ticket[ticket_id] or {}
        table.insert(by_ticket[ticket_id], uuid)
        all_uuids[uuid] = true
    end

    for _, row in ipairs(rows([[SELECT r.ticket_id, rs.agent_session_uuid
                                FROM run_steps rs
                                JOIN runs r ON r.id = rs.run_id
                                WHERE rs.agent_session_uuid IS NOT NULL
                                  AND rs.agent_session_uuid != '']] .. ticket_filter, params)) do
        add(row.ticket_id, row.agent_session_uuid)
    end

    local event_filter, event_params = id_filter("ticket_id", ticket_ids)
    for _, event in ipairs(rows([[SELECT ticket_id, payload
                                  FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind = 'ticket.manual_session_removed']] .. event_filter, event_params)) do
        local payload = util.decode(event.payload, {})
        if not util.is_blank(event.ticket_id) and not util.is_blank(payload.session_uuid) then
            removed_by_ticket[event.ticket_id] = removed_by_ticket[event.ticket_id] or {}
            removed_by_ticket[event.ticket_id][payload.session_uuid] = true
        end
    end
    for _, event in ipairs(rows([[SELECT ticket_id, payload
                                  FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind IN ('ticket.merge_requested',
                                                 'ticket.merge_agent_linked',
                                                 'ticket.manual_session_linked',
                                                 'question.agent_linked')]] .. event_filter, event_params)) do
        local payload = util.decode(event.payload, {})
        if not (removed_by_ticket[event.ticket_id] and removed_by_ticket[event.ticket_id][payload.session_uuid]) then
            add(event.ticket_id, payload.session_uuid)
        end
    end

    return by_ticket, all_uuids
end

function M.ticket_session_links_for_uuids(session_uuids)
    if type(session_uuids) ~= "table" or #session_uuids == 0 then
        return {}
    end

    local wanted = {}
    for _, uuid in ipairs(session_uuids) do
        if not util.is_blank(uuid) then
            wanted[uuid] = true
        end
    end

    local uuid_filter, params = id_filter("rs.agent_session_uuid", session_uuids)
    local links = {}
    local seen = {}
    local removed_by_ticket = {}

    local function add(uuid, ticket)
        if util.is_blank(uuid) or not ticket or util.is_blank(ticket.id) then
            return
        end
        seen[uuid] = seen[uuid] or {}
        if seen[uuid][ticket.id] then
            return
        end
        seen[uuid][ticket.id] = true
        links[uuid] = links[uuid] or {}
        table.insert(links[uuid], ticket)
    end

    for _, event in ipairs(rows([[SELECT ticket_id, payload
                                  FROM events
                                  WHERE ticket_id IS NOT NULL
                                    AND kind = 'ticket.manual_session_removed']])) do
        local payload = util.decode(event.payload, {})
        if wanted[payload.session_uuid] then
            removed_by_ticket[event.ticket_id] = removed_by_ticket[event.ticket_id] or {}
            removed_by_ticket[event.ticket_id][payload.session_uuid] = true
        end
    end

    for _, row in ipairs(rows([[SELECT rs.agent_session_uuid, t.*
                                FROM run_steps rs
                                JOIN runs r ON r.id = rs.run_id
                                JOIN tickets t ON t.id = r.ticket_id
                                WHERE rs.agent_session_uuid IS NOT NULL
                                  AND rs.agent_session_uuid != '']] .. uuid_filter .. [[
                                ORDER BY t.updated_at DESC, t.created_at DESC]], params)) do
        local uuid = row.agent_session_uuid
        row.agent_session_uuid = nil
        add(uuid, row)
    end

    for _, event in ipairs(rows([[SELECT e.ticket_id, e.payload, t.*
                                  FROM events e
                                  JOIN tickets t ON t.id = e.ticket_id
                                  WHERE e.ticket_id IS NOT NULL
                                    AND e.kind IN ('ticket.merge_requested',
                                                   'ticket.merge_agent_linked',
                                                   'ticket.manual_session_linked',
                                                   'question.agent_linked')
                                  ORDER BY t.updated_at DESC, t.created_at DESC]])) do
        local payload = util.decode(event.payload, {})
        local uuid = payload.session_uuid
        if wanted[uuid] and not (removed_by_ticket[event.ticket_id] and removed_by_ticket[event.ticket_id][uuid]) then
            event.payload = nil
            add(uuid, event)
        end
    end

    return links
end

function M.ticket_detail_overview(ticket_id)
    local runs = M.ticket_runs(ticket_id)
    local latest_run = runs[1]
    local open_run = nil
    local run_ids = {}
    local pipeline_ids = {}
    for _, run in ipairs(runs) do
        table.insert(run_ids, run.id)
        pipeline_ids[run.pipeline_id] = true
        if not open_run and (run.status == "active" or run.status == "blocked") then
            open_run = run
        end
    end

    local pipelines = M.list_pipelines()
    local pipelines_by_id = index_by_id(pipelines)
    local referenced_pipeline_ids = {}
    for pipeline_id, _ in pairs(pipeline_ids) do
        if not pipelines_by_id[pipeline_id] then
            referenced_pipeline_ids[#referenced_pipeline_ids + 1] = pipeline_id
        end
    end
    if #referenced_pipeline_ids > 0 then
        local referenced = rows(
            "SELECT * FROM pipelines WHERE id IN (" .. placeholders(#referenced_pipeline_ids) .. ")",
            referenced_pipeline_ids
        )
        for _, pipeline in ipairs(referenced) do
            pipelines_by_id[pipeline.id] = pipeline
        end
    end
    local steps_by_id = index_by_id(rows("SELECT * FROM pipeline_steps"))

    local run_steps = {}
    if #run_ids > 0 then
        run_steps = rows([[SELECT rs.*, r.ticket_id, r.pipeline_id, ps.name, ps.kind, ps.position, ps.agent_name, ps.prompt, ps.command
                           FROM run_steps rs
                           JOIN runs r ON r.id = rs.run_id
                           JOIN pipeline_steps ps ON ps.id = rs.step_id
                           WHERE rs.run_id IN (]] .. placeholders(#run_ids) .. [[)
                           ORDER BY r.created_at DESC, COALESCE(rs.sequence, 0) ASC, rs.created_at ASC, rs.id ASC]], run_ids)
    end
    local run_steps_by_run = group_by(run_steps, "run_id")

    local gate_results_by_run_step = {}
    local reviews_by_run_step = {}
    if #run_steps > 0 then
        local run_step_ids = {}
        for _, step in ipairs(run_steps) do
            table.insert(run_step_ids, step.id)
        end
        gate_results_by_run_step = group_by(rows("SELECT * FROM gate_results WHERE run_step_id IN (" .. placeholders(#run_step_ids) .. ") ORDER BY created_at ASC, id ASC", run_step_ids), "run_step_id")
        reviews_by_run_step = group_by(rows("SELECT * FROM reviews WHERE run_step_id IN (" .. placeholders(#run_step_ids) .. ") ORDER BY created_at DESC, id DESC", run_step_ids), "run_step_id")
    end

    local events = M.ticket_events(ticket_id, nil, 100)
    local events_by_kind = group_by(events, "kind")
    local session_uuids_by_ticket = M.ticket_session_uuids_by_ticket({ ticket_id })

    return {
        runs = runs,
        latest_run = latest_run,
        open_run = open_run,
        pipelines = pipelines,
        pipelines_by_id = pipelines_by_id,
        steps_by_id = steps_by_id,
        run_steps = run_steps,
        run_steps_by_run = run_steps_by_run,
        gate_results_by_run_step = gate_results_by_run_step,
        reviews_by_run_step = reviews_by_run_step,
        questions = M.ticket_questions(ticket_id),
        open_questions = M.ticket_questions(ticket_id, "open"),
        dependencies = M.ticket_dependencies(ticket_id),
        visible_tickets = M.visible_tickets(),
        events = events,
        events_by_kind = events_by_kind,
        merge_events = events_by_kind["ticket.merge_requested"] or {},
        failed_merge_events = events_by_kind["ticket.merge_request_failed"] or {},
        session_uuids = session_uuids_by_ticket[ticket_id] or {},
    }
end

function M.get_run_step(run_id, step_id)
    return first([[SELECT * FROM run_steps
                   WHERE run_id = ? AND step_id = ?
                   ORDER BY COALESCE(sequence, 0) DESC, created_at DESC, id DESC
                   LIMIT 1]], run_id, step_id)
end

function M.update_run_step(run_id, step_id, attrs)
    local run = M.get_run(run_id)
    local run_step = run and run.current_run_step_id and db.run_steps:where{ id = run.current_run_step_id } or nil
    if not run_step or run_step.step_id ~= step_id then
        run_step = M.get_run_step(run_id, step_id)
    end
    if not run_step then
        return nil
    end
    return M.update_run_step_visit(run_step.id, attrs)
end

function M.get_run_step_visit(run_step_id)
    return db.run_steps:where{ id = run_step_id }
end

function M.update_run_step_visit(run_step_id, attrs)
    local set = util.copy(attrs or {})
    set.updated_at = util.now()
    db.run_steps:update{ where = { id = run_step_id }, set = set }
    local run_step = M.get_run_step_visit(run_step_id)
    publish_entity("run_step", run_step)
    if run_step then
        local run = M.get_run(run_step.run_id)
        if run then
            publish_entity("run", run)
            publish_entity("ticket", M.get_ticket(run.ticket_id))
        end
    end
    return run_step
end

function M.next_run_step_sequence(run_id)
    local row = first("SELECT MAX(sequence) AS sequence FROM run_steps WHERE run_id = ?", run_id)
    return tonumber(row and row.sequence or 0) + 1
end

function M.create_run_step_visit(run_id, step_id, attrs)
    attrs = attrs or {}
    local now = util.now()
    local visit = {
        id = attrs.id or util.id("run_step"),
        run_id = run_id,
        step_id = step_id,
        sequence = attrs.sequence or M.next_run_step_sequence(run_id),
        status = attrs.status or "active",
        agent_session_uuid = attrs.agent_session_uuid,
        started_at = attrs.started_at or now,
        completed_at = attrs.completed_at,
        created_at = now,
        updated_at = now,
    }
    db.run_steps:insert(visit)
    publish_entity("run_step", visit)
    local run = M.get_run(run_id)
    if run then
        publish_entity("ticket", M.get_ticket(run.ticket_id))
    end
    return visit
end

function M.latest_step_session(run_id, step_id)
    return first([[SELECT * FROM run_steps
                   WHERE run_id = ? AND step_id = ? AND agent_session_uuid IS NOT NULL AND agent_session_uuid != ''
                   ORDER BY COALESCE(sequence, 0) DESC, created_at DESC, id DESC
                   LIMIT 1]], run_id, step_id)
end

function M.latest_review_for_run_step(run_step_id)
    if util.is_blank(run_step_id) then
        return nil
    end
    return first("SELECT * FROM reviews WHERE run_step_id = ? ORDER BY created_at DESC, id DESC LIMIT 1", run_step_id)
end

function M.next_step(run, completed_step, completed_run_step_id)
    local current = run.current_step_id
    local steps = M.pipeline_steps(run.pipeline_id)
    if current == nil or current == "" then
        return steps[1]
    end
    for index, step in ipairs(steps) do
        if step.id == current then
            local current_step = completed_step or step
            local review = M.latest_review_for_run_step(completed_run_step_id or run.current_run_step_id)
            if review then
                if review.verdict == "approved" and not util.is_blank(current_step.on_approved_step_id) then
                    return M.get_step(current_step.on_approved_step_id)
                end
                if review.verdict == "changes_required" and not util.is_blank(current_step.on_changes_requested_step_id) then
                    return M.get_step(current_step.on_changes_requested_step_id)
                end
                if review.verdict == "blocked" then
                    local target = current_step.on_blocked_step_id or current_step.on_changes_requested_step_id
                    if not util.is_blank(target) then
                        return M.get_step(target)
                    end
                end
                if review.verdict == "changes_required" or review.verdict == "blocked" then
                    return nil, {
                        status = "missing_transition",
                        verdict = review.verdict,
                        review_id = review.id,
                        step_id = current_step.id,
                    }
                end
            end
            if step.next_step_id and step.next_step_id ~= "" then
                return M.get_step(step.next_step_id)
            end
            return steps[index + 1]
        end
    end
    return nil
end

function M.submit_gate(attrs)
    util.assert_present(attrs.run_id, "run_id")
    util.assert_present(attrs.step_id, "step_id")
    util.assert_present(attrs.gate_id, "gate_id")
    local run = M.get_run(attrs.run_id)
    local run_step_id = attrs.run_step_id or (run and run.current_run_step_id)
    local result = {
        id = attrs.id or util.id("gate_result"),
        run_id = attrs.run_id,
        run_step_id = run_step_id,
        step_id = attrs.step_id,
        gate_id = attrs.gate_id,
        status = attrs.status or "passed",
        summary = attrs.summary or "",
        evidence = util.encode(attrs.evidence or {}),
        created_by_session_uuid = attrs.created_by_session_uuid,
        created_at = util.now(),
    }
    db.gate_results:insert(result)
    publish_entity("gate_result", result)
    M.append_event("gate.submitted", {
        run_id = result.run_id,
        payload = {
            run_step_id = result.run_step_id,
            step_id = result.step_id,
            gate_id = result.gate_id,
            status = result.status,
            summary = result.summary,
        },
    })
    return result
end

function M.gate_results(run_id, gate_id)
    return rows("SELECT * FROM gate_results WHERE run_id = ? AND gate_id = ? ORDER BY created_at DESC", run_id, gate_id)
end

function M.latest_gate_result(run_id, gate_id, run_step_id)
    if not util.is_blank(run_step_id) then
        return first("SELECT * FROM gate_results WHERE run_id = ? AND gate_id = ? AND run_step_id = ? ORDER BY created_at DESC, id DESC LIMIT 1", run_id, gate_id, run_step_id)
    end
    return first("SELECT * FROM gate_results WHERE run_id = ? AND gate_id = ? ORDER BY created_at DESC, id DESC LIMIT 1", run_id, gate_id)
end

function M.create_review(attrs)
    local run = M.get_run(attrs.run_id)
    local review = {
        id = attrs.id or util.id("review"),
        run_id = util.assert_present(attrs.run_id, "run_id"),
        run_step_id = attrs.run_step_id or (run and run.current_run_step_id),
        step_id = util.assert_present(attrs.step_id, "step_id"),
        reviewer_session_uuid = attrs.reviewer_session_uuid,
        verdict = attrs.verdict or "changes_required",
        summary = attrs.summary or "",
        created_at = util.now(),
    }
    db.reviews:insert(review)
    publish_entity("review", review)
    for _, finding in ipairs(attrs.findings or {}) do
        local finding_row = {
            id = finding.id or util.id("finding"),
            review_id = review.id,
            run_id = review.run_id,
            step_id = review.step_id,
            severity = finding.severity or "medium",
            title = util.assert_present(finding.title, "finding.title"),
            file = finding.file,
            line = finding.line,
            details = finding.details or "",
            suggested_fix = finding.suggested_fix or "",
            status = finding.status or "open",
            resolution = finding.resolution,
            created_at = util.now(),
            updated_at = util.now(),
        }
        db.review_findings:insert(finding_row)
        publish_entity("finding", finding_row)
    end
    M.append_event("review.submitted", {
        run_id = review.run_id,
        payload = { review_id = review.id, run_step_id = review.run_step_id, verdict = review.verdict, summary = review.summary },
    })
    return review
end

function M.run_reviews(run_id)
    return rows("SELECT * FROM reviews WHERE run_id = ? ORDER BY created_at DESC", run_id)
end

function M.run_step_reviews(run_step_id)
    if util.is_blank(run_step_id) then
        return {}
    end
    return rows("SELECT * FROM reviews WHERE run_step_id = ? ORDER BY created_at DESC", run_step_id)
end

function M.run_findings(run_id)
    return rows("SELECT * FROM review_findings WHERE run_id = ? ORDER BY created_at DESC", run_id)
end

function M.open_findings(run_id)
    return rows("SELECT * FROM review_findings WHERE run_id = ? AND status = 'open' ORDER BY created_at DESC", run_id)
end

function M.open_blocking_findings(run_id)
    return rows([[SELECT * FROM review_findings
                  WHERE run_id = ? AND status = 'open' AND severity IN ('blocker', 'high')
                  ORDER BY created_at ASC]], run_id)
end

function M.resolve_finding(finding_id, attrs)
    db.review_findings:update{
        where = { id = finding_id },
        set = {
            status = attrs.status or "resolved",
            resolution = attrs.resolution or attrs.summary or "Resolved",
            updated_at = util.now(),
        },
    }
    local finding = db.review_findings:where{ id = finding_id }
    publish_entity("finding", finding)
    M.append_event("finding.resolved", {
        run_id = finding and finding.run_id or nil,
        payload = { finding_id = finding_id, resolution = attrs.resolution or attrs.summary },
    })
    return finding
end

function M.close_ticket(ticket_id, attrs)
    local now = util.now()
    db.tickets:update{
        where = { id = ticket_id },
        set = { status = "closed", updated_at = now },
    }
    for _, run in ipairs(M.ticket_runs(ticket_id)) do
        db.runs:update{
            where = { id = run.id },
            set = { status = "closed", updated_at = now },
        }
        publish_entity("run", M.get_run(run.id))
    end
    publish_entity("ticket", M.get_ticket(ticket_id))
    M.append_event("ticket.closed", {
        ticket_id = ticket_id,
        payload = attrs or {},
    })
    return M.get_ticket(ticket_id)
end

function M.add_artifact(attrs)
    local run = M.get_run(attrs.run_id)
    local artifact = {
        id = attrs.id or util.id("artifact"),
        run_id = util.assert_present(attrs.run_id, "run_id"),
        run_step_id = attrs.run_step_id or (run and run.current_run_step_id),
        step_id = attrs.step_id,
        kind = attrs.kind or "note",
        uri = attrs.uri,
        summary = attrs.summary or "",
        payload = util.encode(attrs.payload or {}),
        created_at = util.now(),
    }
    db.artifacts:insert(artifact)
    publish_entity("artifact", artifact)
    if run then
        publish_entity("ticket", M.get_ticket(run.ticket_id))
    end
    M.append_event("artifact.added", { run_id = artifact.run_id, payload = artifact })
    return artifact
end

function M.run_artifacts(run_id)
    return rows("SELECT * FROM artifacts WHERE run_id = ? ORDER BY created_at DESC", run_id)
end

function M.create_question(attrs)
    util.assert_present(attrs.ticket_id, "ticket_id")
    util.assert_present(attrs.question, "question")
    local now = util.now()
    local question = {
        id = attrs.id or util.id("question"),
        ticket_id = attrs.ticket_id,
        run_id = attrs.run_id,
        run_step_id = attrs.run_step_id,
        step_id = attrs.step_id,
        kind = attrs.kind or "human",
        status = attrs.status or "open",
        question = attrs.question,
        answer = attrs.answer,
        asked_by_session_uuid = attrs.asked_by_session_uuid,
        answered_by_session_uuid = attrs.answered_by_session_uuid,
        advisor_session_uuid = attrs.advisor_session_uuid,
        blocking = attrs.blocking and 1 or 0,
        created_at = now,
        updated_at = now,
    }
    db.questions:insert(question)
    publish_entity("question", question)
    publish_entity("ticket", M.get_ticket(question.ticket_id))
    M.append_event("question.created", {
        run_id = question.run_id,
        ticket_id = question.ticket_id,
        payload = {
            question_id = question.id,
            kind = question.kind,
            blocking = question.blocking,
        },
    })
    return question
end

function M.update_question(question_id, attrs)
    local question = db.questions:where{ id = question_id }
    if not question then
        error("question not found: " .. tostring(question_id))
    end
    local set = {}
    for _, field in ipairs({ "status", "answer", "answered_by_session_uuid", "advisor_session_uuid" }) do
        if attrs and attrs[field] ~= nil then
            set[field] = attrs[field]
        end
    end
    if not has_fields(set) then
        return question
    end
    set.updated_at = util.now()
    db.questions:update{ where = { id = question_id }, set = set }
    local updated = db.questions:where{ id = question_id }
    publish_entity("question", updated)
    publish_entity("ticket", M.get_ticket(updated.ticket_id))
    M.append_event("question.updated", {
        run_id = updated.run_id,
        ticket_id = updated.ticket_id,
        payload = {
            question_id = question_id,
            status = updated.status,
            answered = not util.is_blank(updated.answer),
        },
    })
    return updated
end

function M.get_question(question_id)
    return db.questions:where{ id = question_id }
end

function M.ticket_questions(ticket_id, status)
    if util.is_blank(status) then
        return rows("SELECT * FROM questions WHERE ticket_id = ? ORDER BY created_at DESC", ticket_id)
    end
    return rows("SELECT * FROM questions WHERE ticket_id = ? AND status = ? ORDER BY created_at DESC", ticket_id, status)
end

function M.question_answers(filters)
    filters = filters or {}
    local where = { "status <> 'open'" }
    local params = {}
    local function add(field, value)
        if not util.is_blank(value) then
            table.insert(where, field .. " = ?")
            table.insert(params, value)
        end
    end
    add("id", filters.question_id)
    add("ticket_id", filters.ticket_id)
    add("run_id", filters.run_id)
    add("asked_by_session_uuid", filters.asked_by_session_uuid)
    add("status", filters.status)
    local sql = "SELECT * FROM questions WHERE " .. table.concat(where, " AND ") .. " ORDER BY updated_at DESC"
    if #params == 0 then
        return rows(sql)
    end
    return rows(sql, params)
end

function M.open_questions()
    return rows("SELECT * FROM questions WHERE status = 'open' ORDER BY created_at DESC")
end

function M.has_open_questions()
    local row = first("SELECT 1 AS found FROM questions WHERE status = 'open' LIMIT 1")
    return row ~= nil
end

function M.open_questions_with_tickets()
    return rows([[SELECT q.*, t.title AS ticket_title
                  FROM questions q
                  LEFT JOIN tickets t ON t.id = q.ticket_id
                  WHERE q.status = 'open'
                  ORDER BY q.created_at DESC]])
end

function M.run_detail_overview(run_id)
    local run = M.get_run(run_id)
    if not run then
        return nil
    end
    return {
        run = run,
        ticket = M.get_ticket(run.ticket_id),
        pipeline = M.get_pipeline(run.pipeline_id),
        steps = M.run_steps(run.id),
        reviews = M.run_reviews(run.id),
        findings = M.run_findings(run.id),
        artifacts = M.run_artifacts(run.id),
        events = M.run_events(run.id, 12),
    }
end

function M.run_events(run_id, limit)
    return rows("SELECT * FROM events WHERE run_id = ? ORDER BY created_at DESC LIMIT ?", run_id, limit or 25)
end

function M.ticket_events(ticket_id, kind, limit)
    if util.is_blank(kind) then
        return rows("SELECT * FROM events WHERE ticket_id = ? ORDER BY created_at DESC LIMIT ?", ticket_id, limit or 25)
    end
    return rows("SELECT * FROM events WHERE ticket_id = ? AND kind = ? ORDER BY created_at DESC LIMIT ?", ticket_id, kind, limit or 25)
end

function M.find_active_assignment(session_uuid)
    if util.is_blank(session_uuid) then
        return nil
    end
    return first([[SELECT rs.run_id, rs.step_id, rs.agent_session_uuid
                   FROM run_steps rs
                   WHERE rs.agent_session_uuid = ? AND rs.status = 'active'
                   ORDER BY rs.started_at DESC LIMIT 1]], session_uuid)
end

return M
