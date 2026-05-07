-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/engine.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local util = require("project_pipelines.util")
local entities = require("project_pipelines.entities")
local Hub = require("lib.hub")
local Agent = require("lib.agent")

local OWNER = "project-pipelines"
local SURFACE = "pipelines"

local function lua_pattern_escape(value)
    return tostring(value):gsub("([^%w])", "%%%1")
end

local function agent_branch_for(ticket, run, step)
    local base = ticket and ticket.id or run.id
    base = tostring(base):gsub("[^%w._-]", "-")
    base = base:gsub("%-+", "-"):gsub("^%-+", ""):gsub("%-+$", "")
    if util.is_blank(base) then
        base = tostring(run.id):gsub("[^%w._-]", "-")
    end
    return "project-pipelines/" .. base
end

local function ticket_branch_for(ticket, fallback)
    local base = ticket and ticket.id or fallback
    base = tostring(base):gsub("[^%w._-]", "-")
    base = base:gsub("%-+", "-"):gsub("^%-+", ""):gsub("%-+$", "")
    if util.is_blank(base) then
        base = tostring(fallback or "ticket"):gsub("[^%w._-]", "-")
    end
    return "project-pipelines/" .. base
end

local function ticket_workspace_name(ticket, fallback)
    local title = ticket and ticket.title or fallback or "Ticket"
    title = tostring(title):gsub("%s+", " "):gsub("^%s+", ""):gsub("%s+$", "")
    if util.is_blank(title) then
        title = tostring(ticket and ticket.id or fallback or "Ticket")
    end
    if #title > 64 then
        title = title:sub(1, 61):gsub("%s+$", "") .. "..."
    end
    return "Pipeline - " .. title
end

local M = {}

function M.register_entities()
    entities.register()
end

function M.publish_entity_snapshots()
    entities.publish_snapshots()
end

local function refresh_surfaces(ctx)
    -- Data changes now flow through plugin-owned entity frames. Route/tree
    -- snapshots remain structural and are sent only by the generic surface
    -- subscription path, not by Project Pipelines mutators.
    return ctx
end

local function step_gate_status(run, step, gate)
    if gate.kind == "review_clear" then
        local review = repo.latest_review_for_run_step(run.current_run_step_id)
        if review and (review.verdict == "changes_required" or review.verdict == "blocked") then
            return { satisfied = true, status = "passed", source = "review_transition", review = review }
        end
        local blockers = repo.open_blocking_findings(run.id)
        if #blockers == 0 then
            return { satisfied = true, status = "passed", source = "review_clear" }
        end
        return { satisfied = false, status = "blocked", source = "review_clear", findings = blockers }
    end

    local latest = repo.latest_gate_result(run.id, gate.id, run.current_run_step_id)
    if latest and latest.status == "passed" then
        local missing = {}
        local evidence = util.decode(latest.evidence, {})
        for _, field in ipairs(util.decode(gate.required_fields, {})) do
            if field == "summary" then
                if util.is_blank(latest.summary) then
                    table.insert(missing, field)
                end
            elseif field == "evidence" then
                if type(evidence) ~= "table" or next(evidence) == nil then
                    table.insert(missing, field)
                end
            elseif type(evidence) ~= "table" or util.is_blank(evidence[field]) then
                table.insert(missing, field)
            end
        end
        if #missing > 0 then
            return { satisfied = false, status = "missing_required_fields", result = latest, missing_fields = missing }
        end
        return { satisfied = true, status = latest.status, result = latest }
    end
    if latest and latest.status == "failed" then
        return { satisfied = false, status = latest.status, result = latest }
    end
    return { satisfied = false, status = latest and latest.status or "missing", result = latest }
end

local function unmet_gates(run, step)
    local unmet = {}
    for _, gate in ipairs(repo.step_gates(step.id)) do
        local status = step_gate_status(run, step, gate)
        if not status.satisfied then
            table.insert(unmet, {
                id = gate.id,
                kind = gate.kind,
                prompt = gate.prompt,
                required_fields = util.decode(gate.required_fields, {}),
                status = status.status,
                missing_fields = status.missing_fields,
                finding_count = status.findings and #status.findings or nil,
            })
        end
    end
    return unmet
end

local function has_review_kickback(run, step)
    local review = repo.latest_review_for_run_step(run.current_run_step_id)
    if not review then
        return false
    end
    if review.verdict == "changes_required" then
        return not util.is_blank(step.on_changes_requested_step_id)
    end
    if review.verdict == "blocked" then
        return not util.is_blank(step.on_blocked_step_id or step.on_changes_requested_step_id)
    end
    return false
end

local function context_from_params(params, context)
    local session_uuid = context and context.session_uuid or params.session_uuid
    local run_id = params.run_id
    local assignment = nil
    if util.is_blank(run_id) and not util.is_blank(session_uuid) then
        assignment = repo.find_active_assignment(session_uuid)
        run_id = assignment and assignment.run_id or nil
    end

    local run = not util.is_blank(run_id) and repo.get_run(run_id) or nil
    local ticket_id = params.ticket_id or (run and run.ticket_id)
    local ticket = not util.is_blank(ticket_id) and repo.get_ticket(ticket_id) or nil
    if not ticket then
        error("ticket_id or active pipeline assignment is required")
    end

    local run_step = run and run.current_run_step_id and repo.get_run_step_visit(run.current_run_step_id) or nil
    return {
        session_uuid = session_uuid,
        assignment = assignment,
        run = run,
        ticket = ticket,
        run_step = run_step,
        step_id = run_step and run_step.step_id or (assignment and assignment.step_id),
    }
end

function M.context_for(run_id, session_uuid)
    local assignment = nil
    if util.is_blank(run_id) then
        assignment = repo.find_active_assignment(session_uuid)
        run_id = assignment and assignment.run_id or nil
    end
    util.assert_present(run_id, "run_id")

    local run = repo.get_run(run_id)
    if not run then
        error("run not found: " .. tostring(run_id))
    end
    local ticket = repo.get_ticket(run.ticket_id)
    local pipeline = repo.get_pipeline(run.pipeline_id)
    local current_step = run.current_step_id and repo.get_step(run.current_step_id) or nil
    local current_run_step = run.current_run_step_id and repo.get_run_step_visit(run.current_run_step_id) or nil
    local gates = current_step and repo.step_gates(current_step.id) or {}
    local visible_gates = {}
    for _, gate in ipairs(gates) do
        table.insert(visible_gates, {
            id = gate.id,
            kind = gate.kind,
            prompt = gate.prompt,
            required_fields = util.decode(gate.required_fields, {}),
            latest_result = repo.latest_gate_result(run.id, gate.id, run.current_run_step_id),
        })
    end

    return {
        run = run,
        ticket = ticket,
        pipeline = pipeline,
        current_step = current_step,
        current_run_step = current_run_step,
        gates = visible_gates,
        run_steps = repo.run_steps(run.id),
        reviews = repo.run_reviews(run.id),
        current_step_reviews = repo.run_step_reviews(run.current_run_step_id),
        latest_review_for_current_step = repo.latest_review_for_run_step(run.current_run_step_id),
        findings = repo.run_findings(run.id),
        open_findings = repo.open_findings(run.id),
        artifacts = repo.run_artifacts(run.id),
        open_questions = repo.ticket_questions(run.ticket_id, "open"),
        questions = repo.ticket_questions(run.ticket_id),
        question_answers = repo.question_answers({ run_id = run.id, asked_by_session_uuid = session_uuid }),
        dependencies = repo.ticket_dependencies(run.ticket_id),
        blocking_dependencies = repo.blocking_ticket_dependencies(run.ticket_id),
        recent_events = repo.run_events(run.id, 25),
        assignment = assignment,
    }
end

local function spawn_step_agent(run, step)
    if util.is_blank(step.agent_name) then
        return nil, "step has no agent_name"
    end
    if util.is_blank(run.target_id) and util.is_blank(run.target_path) then
        return nil, "target_id or target_path is required before spawning agent steps"
    end

    local ticket = repo.get_ticket(run.ticket_id)
    local current_visit = run.current_run_step_id and repo.get_run_step_visit(run.current_run_step_id) or nil
    local existing = repo.latest_step_session(run.id, step.id)
    local role_prompt = table.concat({
        "You are running a Botster project pipeline step.",
        "Pipeline step: " .. step.name .. " (" .. step.id .. ")",
        "Ticket: " .. (ticket and ticket.title or run.ticket_id),
        "Role instructions: " .. (step.prompt or ""),
        "Use the project_pipelines_current_context MCP tool to inspect ticket, run, gates, reviews, artifacts, findings, questions, and question answers.",
        "State assumptions explicitly in artifacts/reviews. If the ticket has multiple plausible meanings or you would need to ignore/waive part of it, ask a human question instead of choosing silently.",
        "Prefer the smallest surgical change that satisfies the ticket intent. Do not add speculative abstractions, optional configurability, broad refactors, or adjacent cleanup unless the ticket requires them.",
        "Define and preserve verifiable success criteria. Every changed line should trace to the ticket intent, a required convention, or cleanup made necessary by your own change.",
        "Prove the actual runtime or user path changed. Evidence that code exists is not enough; identify where the production entry point uses the new behavior, or document why the ticket is intentionally scaffold-only.",
        "If you need human help, call project_pipelines_ask_human with a concise blocking question, then wait for a Project Pipelines notification and call project_pipelines_receive_question_answers.",
        "If you need another agent's opinion, call project_pipelines_ask_agent with the specific question and context you want reviewed, then wait for a Project Pipelines notification and call project_pipelines_receive_question_answers.",
        "When work is ready for advancement, submit the required gate evidence with project_pipelines_submit_gate, then call project_pipelines_request_step_advance.",
        "If you are reviewing, check correctness, regressions, architecture fit, missing tests, documentation gaps, overcomplication, hidden assumptions, dead code, deprecated code paths, and unwired implementation. Do not accept pre-existing failures as a blanket excuse; require exact evidence that a failure is unrelated or send the work back. Call project_pipelines_submit_review with findings or approval.",
    }, "\n\n")

    if existing and existing.agent_session_uuid and Agent.get(existing.agent_session_uuid) then
        local ok, err = pcall(function()
            Hub.get():post(existing.agent_session_uuid, {
                type = "task",
                from_agent_id = OWNER,
                from_label = "Project Pipelines",
                payload = {
                    run_id = run.id,
                    ticket_id = run.ticket_id,
                    step_id = step.id,
                    instructions = role_prompt,
                },
            })
            Hub.get():notify(existing.agent_session_uuid, {
                source = OWNER,
                title = "Pipeline step returned",
                body = "Ticket " .. tostring(ticket and ticket.title or run.ticket_id) .. " is back at " .. tostring(step.name) .. ".",
                action = {
                    kind = "mcp_tool",
                    name = "project_pipelines_current_context",
                    params = { run_id = run.id },
                },
            })
        end)
        repo.append_event("step.agent_prompted", {
            run_id = run.id,
            ticket_id = run.ticket_id,
            payload = {
                step_id = step.id,
                session_uuid = existing.agent_session_uuid,
                delivered = ok,
                error = ok and nil or tostring(err),
            },
        })
        if current_visit then
            repo.update_run_step_visit(current_visit.id, { agent_session_uuid = existing.agent_session_uuid })
        end
        return { session_uuid = existing.agent_session_uuid, reused = true }, nil
    end

    local request_id = string.format("%s:%s:%s:agent", OWNER, run.id, step.id)
    local ok, created = pcall(function()
        return Hub.get():create_agent{
            request_id = request_id,
            agent_name = step.agent_name,
            issue_or_branch = agent_branch_for(ticket, run, step),
            target_id = run.target_id,
            target_path = run.target_path,
            workspace_id = run.workspace_id,
            workspace_name = run.workspace_name or ticket_workspace_name(ticket, run.id),
            label = step.name .. " - " .. (ticket and ticket.title or run.id),
            prompt = role_prompt,
            metadata = {
                owner_plugin = OWNER,
                visibility = "plugin",
                surface = SURFACE,
                ticket_id = run.ticket_id,
                run_id = run.id,
                step_id = step.id,
                pipeline_id = run.pipeline_id,
                role = step.id,
            },
        }
    end)

    if not ok then
        return nil, tostring(created)
    end

    repo.append_event("step.agent_requested", {
        run_id = run.id,
        ticket_id = run.ticket_id,
        payload = {
            step_id = step.id,
            agent_name = step.agent_name,
            request_id = request_id,
            status = created and created.status or "queued",
        },
    })
    return {
        ok = true,
        request_id = request_id,
        status = created and created.status or "queued",
    }, nil
end

function M.ask_human(params, context)
    local resolved = context_from_params(params or {}, context or {})
    local question = repo.create_question{
        ticket_id = resolved.ticket.id,
        run_id = resolved.run and resolved.run.id or nil,
        run_step_id = resolved.run_step and resolved.run_step.id or nil,
        step_id = resolved.step_id,
        kind = "human",
        question = util.assert_present(params.question, "question"),
        asked_by_session_uuid = resolved.session_uuid,
        blocking = params.blocking ~= false,
    }
    refresh_surfaces()
    return question
end

function M.ask_agent(params, context)
    local resolved = context_from_params(params or {}, context or {})
    local question = repo.create_question{
        ticket_id = resolved.ticket.id,
        run_id = resolved.run and resolved.run.id or nil,
        run_step_id = resolved.run_step and resolved.run_step.id or nil,
        step_id = resolved.step_id,
        kind = "agent",
        question = util.assert_present(params.question, "question"),
        asked_by_session_uuid = resolved.session_uuid,
        blocking = params.blocking == true,
    }

    local prompt_parts = {
        "You are a Project Pipelines question advisor.",
        "Question ID: " .. question.id,
        "Ticket: " .. resolved.ticket.title,
        "Question:",
        question.question,
        "Use project_pipelines_get_ticket and project_pipelines_current_context if you need more context.",
        "Answer by calling project_pipelines_answer_question with this question_id. Keep the answer direct and actionable.",
    }
    if resolved.run then
        table.insert(prompt_parts, 4, "Run: " .. resolved.run.id)
    end
    local prompt = table.concat(prompt_parts, "\n\n")

    local request_id = string.format("%s:%s:question:%s:agent", OWNER, resolved.ticket.id, question.id)
    local ok, created = pcall(function()
        return Hub.get():create_agent{
            request_id = request_id,
            agent_name = params.agent_name or "claude",
            issue_or_branch = ticket_branch_for(resolved.ticket, question.id),
            target_id = resolved.ticket.target_id,
            target_path = resolved.ticket.target_path,
            workspace_id = resolved.run and resolved.run.workspace_id or params.workspace_id,
            workspace_name = params.workspace_name or (resolved.run and resolved.run.workspace_name) or ticket_workspace_name(resolved.ticket, question.id),
            label = "Question - " .. resolved.ticket.title,
            prompt = prompt,
            metadata = {
                owner_plugin = OWNER,
                visibility = "plugin",
                surface = SURFACE,
                ticket_id = resolved.ticket.id,
                run_id = resolved.run and resolved.run.id or nil,
                question_id = question.id,
                role = "question_advisor",
            },
        }
    end)
    if not ok then
        repo.update_question(question.id, { status = "open", answer = "Advisor spawn failed: " .. tostring(created) })
        error(tostring(created))
    end

    repo.append_event("question.agent_requested", {
        run_id = resolved.run and resolved.run.id or nil,
        ticket_id = resolved.ticket.id,
        payload = {
            question_id = question.id,
            request_id = request_id,
            status = created and created.status or "queued",
        },
    })
    refresh_surfaces()
    return question
end

function M.answer_question(params, context)
    util.assert_present(params.question_id, "question_id")
    util.assert_present(params.answer, "answer")
    local question = repo.update_question(params.question_id, {
        status = params.status or "answered",
        answer = params.answer,
        answered_by_session_uuid = context and context.session_uuid or params.answered_by_session_uuid,
    })
    if question and not util.is_blank(question.asked_by_session_uuid) then
        local ok, err = pcall(function()
            Hub.get():notify(question.asked_by_session_uuid, {
                source = "project-pipelines",
                title = question.status == "dismissed" and "Question dismissed" or "Question answered",
                body = "A Project Pipelines question was updated for ticket " .. tostring(question.ticket_id) .. ".",
                action = {
                    kind = "mcp_tool",
                    name = "project_pipelines_receive_question_answers",
                    params = { question_id = question.id },
                },
            })
        end)
        if not ok then
            log.warn("[project-pipelines] question answer notification failed: " .. tostring(err))
        end
    end
    refresh_surfaces()
    return question
end

function M.question_answers(params, context)
    params = params or {}
    return repo.question_answers({
        ticket_id = params.ticket_id,
        run_id = params.run_id,
        question_id = params.question_id,
        status = params.status,
        asked_by_session_uuid = params.all == true and nil or (params.asked_by_session_uuid or (context and context.session_uuid)),
    })
end

function M.request_merge(params, context)
    local ticket = repo.get_ticket(util.assert_present(params.ticket_id, "ticket_id"))
    if not ticket then
        error("ticket not found: " .. tostring(params.ticket_id))
    end
    local run = repo.latest_ticket_run(ticket.id)
    if not run or run.status ~= "done" then
        error("ticket must have a completed latest run before merge")
    end
    local pipeline = repo.get_pipeline(run.pipeline_id) or {}
    local merge_policy = pipeline.merge_policy or "direct"
    if merge_policy ~= "direct" and merge_policy ~= "pr" then
        merge_policy = "direct"
    end
    for _, event in ipairs(repo.ticket_events(ticket.id, "ticket.merge_requested", 5)) do
        local payload = util.decode(event.payload, {})
        if payload.session_uuid and Agent.get(payload.session_uuid) then
            return {
                ticket = ticket,
                run = run,
                merge_policy = merge_policy,
                agent = Agent.get(payload.session_uuid):info(),
                request_id = payload.request_id,
                status = "already_requested",
            }
        end
    end

    local prompt = table.concat({
        "You are the Project Pipelines merge agent.",
        "Ticket: " .. ticket.title,
        "Run: " .. run.id,
        "Merge policy: " .. merge_policy,
        "You are the final acceptance gate, not just a Git operator. Re-read the ticket title, ticket description, latest run context, final signoff, review findings, artifacts, branch diff, tests, docs, and merge target before merging.",
        "Judge the work against the intent and meaning of the ticket, not only the literal checklist. If the ticket asked for a new architecture, replacement, or cleanup, verify the old architecture, dead paths, deprecated code, stale docs, stale tests, compatibility shims, and contradictory examples are removed or explicitly human-waived.",
        "Reject hidden assumptions, speculative scope, broad unrelated refactors, or overcomplicated implementation unless the ticket or a human-approved waiver justifies them.",
        "Reject weak evidence. Success criteria must be explicit and verified; evidence must prove the actual production/user/runtime path uses the change, not merely that new helpers, modules, or tests exist.",
        "For runtime, async, data-plane, control-plane, UI-routing, permission, or architecture migration work, require production path proof: entry point, actor/mailbox or controller/job/component path, fallback behavior, old direct path removed or test-fenced, and logs/tests/smoke evidence exercising the path.",
        "Treat stub wiring as incomplete. A new mailbox, component, policy, or helper that immediately delegates to the old production path is not accepted unless the ticket explicitly says scaffold-only or a human waived it.",
        "For hot attach/connect/snapshot/input paths, require explicit latency or race evidence when relevant. New or retained fixed sleeps on hot paths require measurement-backed justification.",
        "All actionable review findings must be resolved or explicitly human-waived before merge. Do not accept 'acceptable workaround', 'future follow-up', 'not necessary', or similar reasoning from an agent unless you agree it is outside the ticket intent or a human has waived it through Project Pipelines questions.",
        "Be proactive about the affected surface area. Confirm the implementation is wired into the actual runtime paths, not merely added beside the old behavior.",
        "Use the repo's conventions for merge strategy and verification.",
        merge_policy == "pr"
            and "This pipeline requires a PR. Create or update the PR only through the Botster MCP PR tools. Do not merge directly. Do not use gh, hub, direct GitHub API calls, browser automation, or manual web UI actions for PR creation or PR updates."
            or "This pipeline requires a direct merge to main. If the acceptance check passes, merge according to the repo's direct-merge convention and do not open a PR.",
        merge_policy == "pr"
            and "If the Botster MCP PR tools are unavailable, ask a human question and wait instead of creating the PR another way."
            or "If direct merge is blocked by conflicts or repo state, add a blocker artifact or ask a human question and wait.",
        "If there are conflicts, incomplete intent coverage, ignored findings, dead/deprecated code, unwired implementation, stale documentation, stale tests, or verification failures, do not merge. Add a project_pipelines_add_artifact blocker summary with exact files, lines when available, and verification attempted; ask a human question only when a waiver or product decision is genuinely needed.",
        "Do not accept pre-existing failures unless you prove with exact evidence they are unrelated to this ticket.",
        "After a successful merge or PR creation, add a project_pipelines_add_artifact summary with the merge commit or PR URL.",
        "Close the ticket only when the merge process is genuinely complete by calling project_pipelines_close_ticket with merge_confirmed=true and include merge_commit, pr_url, or merge_summary when available.",
    }, "\n\n")

    local request_id = string.format("%s:%s:merge:agent", OWNER, ticket.id)
    local ok, created = pcall(function()
        return Hub.get():create_agent{
            request_id = request_id,
            agent_name = params.agent_name or "codex",
            issue_or_branch = ticket_branch_for(ticket, run.id),
            target_id = run.target_id or ticket.target_id,
            target_path = run.target_path or ticket.target_path,
            workspace_id = run.workspace_id or params.workspace_id,
            workspace_name = params.workspace_name or run.workspace_name or ticket_workspace_name(ticket, run.id),
            label = "Merge - " .. ticket.title,
            prompt = prompt,
            metadata = {
                owner_plugin = OWNER,
                visibility = "plugin",
                surface = SURFACE,
                ticket_id = ticket.id,
                run_id = run.id,
                role = "merge",
            },
        }
    end)
    if not ok then
        error(tostring(created))
    end

    repo.append_event("ticket.merge_requested", {
        run_id = run.id,
        ticket_id = ticket.id,
        payload = {
            request_id = request_id,
            status = created and created.status or "queued",
            strategy = params.strategy or "agent",
            merge_policy = merge_policy,
        },
    })
    refresh_surfaces(context)
    return { ticket = ticket, run = run, merge_policy = merge_policy, agent = created, request_id = request_id }
end

function M.close_ticket(ticket_id, attrs)
    util.assert_present(ticket_id, "ticket_id")
    attrs = attrs or {}
    local ticket = repo.get_ticket(ticket_id)
    if not ticket then
        error("ticket not found: " .. tostring(ticket_id))
    end
    local latest_run = repo.latest_ticket_run(ticket_id)
    if latest_run and latest_run.status == "done" and attrs.merge_confirmed ~= true then
        error("completed pipeline work must be merged before closing; start merge or close with merge_confirmed=true from the merge agent")
    end

    local closed_sessions = {}
    local errors = {}
    for _, session_uuid in ipairs(repo.ticket_session_uuids(ticket_id)) do
        local ok, result = pcall(function()
            return Hub.get():delete_agent(session_uuid, false)
        end)
        if ok then
            table.insert(closed_sessions, { session_uuid = session_uuid, result = result })
        else
            table.insert(errors, { session_uuid = session_uuid, error = tostring(result) })
        end
    end

    if latest_run and attrs.merge_confirmed == true then
        local summary = attrs.merge_summary or attrs.summary or "Merge confirmed before ticket closure."
        local payload = attrs.merge_payload or {
            merge_confirmed = true,
            merge_commit = attrs.merge_commit,
            pr_url = attrs.pr_url,
            closed_by_session_uuid = attrs.closed_by_session_uuid,
        }
        repo.add_artifact{
            run_id = latest_run.id,
            kind = attrs.merge_kind or "merge",
            uri = attrs.pr_url,
            summary = summary,
            payload = payload,
        }
    end

    local updated = repo.close_ticket(ticket_id, {
        merge_confirmed = attrs.merge_confirmed == true,
        closed_sessions = closed_sessions,
        errors = errors,
        merge_commit = attrs.merge_commit,
        pr_url = attrs.pr_url,
        merge_summary = attrs.merge_summary or attrs.summary,
    })
    refresh_surfaces()
    return { ticket = updated, closed_sessions = closed_sessions, errors = errors }
end

local function run_command_step(run, step)
    local command = step.command
    local command_gate = nil
    for _, gate in ipairs(repo.step_gates(step.id)) do
        if gate.kind == "command" then
            command_gate = gate
            command = gate.command or command
            break
        end
    end
    if util.is_blank(command) then
        return nil, "command step has no command"
    end
    if util.is_blank(run.target_path) then
        return nil, "target_path is required for command steps"
    end

    local request_id = string.format("%s:%s:%s:command:%s", OWNER, run.id, step.id, command_gate and command_gate.id or "step")
    local ok, result = pcall(function()
        return hub.run_command_gate{
            request_id = request_id,
            command = command,
            cwd = run.target_path,
            timeout_secs = 600,
            context = {
                owner_plugin = OWNER,
                run_id = run.id,
                run_step_id = run.current_run_step_id,
                step_id = step.id,
                gate_id = command_gate and command_gate.id or nil,
            },
        }
    end)

    if not ok then
        return nil, tostring(result)
    end
    repo.append_event("step.command_started", {
        run_id = run.id,
        ticket_id = run.ticket_id,
        payload = { step_id = step.id, command = command, request_id = request_id },
    })
    return result, nil
end

function M.activate_step(run, step)
    if not step then
        repo.update_run(run.id, { status = "done", current_step_id = nil, current_run_step_id = nil })
        repo.append_event("run.completed", { run_id = run.id, ticket_id = run.ticket_id, payload = {} })
        local merge_ok, merge_result = pcall(function()
            return M.request_merge({ ticket_id = run.ticket_id }, {})
        end)
        if not merge_ok then
            repo.append_event("ticket.merge_request_failed", {
                run_id = run.id,
                ticket_id = run.ticket_id,
                payload = { error = tostring(merge_result) },
            })
            return { ok = true, status = "done", merge_error = tostring(merge_result) }
        end
        return { ok = true, status = "done", merge = merge_result }
    end

    local now = util.now()
    local visit = repo.create_run_step_visit(run.id, step.id, { status = "active", started_at = now })
    repo.update_run(run.id, { status = "active", current_step_id = step.id, current_run_step_id = visit.id })
    repo.append_event("step.activated", {
        run_id = run.id,
        ticket_id = run.ticket_id,
        payload = { step_id = step.id, run_step_id = visit.id, sequence = visit.sequence, kind = step.kind, name = step.name },
    })

    if step.kind == "agent" then
        local created, err = spawn_step_agent(repo.get_run(run.id), step)
        if err then
            repo.update_run(run.id, { status = "blocked" })
            repo.update_run_step(run.id, step.id, { status = "blocked" })
            repo.append_event("step.spawn_failed", {
                run_id = run.id,
                ticket_id = run.ticket_id,
                payload = { step_id = step.id, error = err },
            })
            return { ok = false, error = err }
        end
        return { ok = true, status = "active", agent = created }
    end

    if step.kind == "command" then
        local result, err = run_command_step(repo.get_run(run.id), step)
        if err then
            repo.update_run(run.id, { status = "blocked" })
            repo.update_run_step(run.id, step.id, { status = "blocked" })
            repo.append_event("step.command_failed_to_start", {
                run_id = run.id,
                ticket_id = run.ticket_id,
                payload = { step_id = step.id, error = err },
            })
            return { ok = false, error = err }
        end
        return { ok = true, status = "active", command = result }
    end

    return { ok = true, status = "active" }
end

function M.start_run(params)
    repo.prune_legacy_seed_data()
    local pipeline_id = params.pipeline_id
    if util.is_blank(pipeline_id) then
        local default = repo.get_default_pipeline()
        pipeline_id = default and default.id or nil
    end
    util.assert_present(pipeline_id, "pipeline_id")
    if not repo.get_ticket(params.ticket_id) then
        error("ticket not found: " .. tostring(params.ticket_id))
    end
    local ticket = repo.get_ticket(params.ticket_id)
    local open_run = repo.open_ticket_run(params.ticket_id)
    if open_run then
        error("ticket already has an open run: " .. tostring(open_run.id))
    end
    if util.is_blank(ticket.target_id) then
        error("ticket target_id is required before starting a run")
    end
    local blockers = repo.blocking_ticket_dependencies(params.ticket_id)
    if #blockers > 0 then
        local names = {}
        for _, blocker in ipairs(blockers) do
            names[#names + 1] = (blocker.depends_on_title or blocker.depends_on_ticket_id) .. " (" .. tostring(blocker.depends_on_status or "open") .. ")"
        end
        error("ticket dependencies must close before starting a run: " .. table.concat(names, ", "))
    end
    local pipeline = repo.get_pipeline(pipeline_id)
    if not pipeline then
        error("pipeline not found: " .. tostring(pipeline_id))
    end
    if #repo.pipeline_steps(pipeline_id) == 0 then
        error("pipeline has no steps: " .. tostring(pipeline_id))
    end

    local run = repo.create_run{
        ticket_id = params.ticket_id,
        pipeline_id = pipeline_id,
        parent_run_id = params.parent_run_id,
        target_id = params.target_id or ticket.target_id,
        target_path = params.target_path or ticket.target_path,
        workspace_id = params.workspace_id,
        workspace_name = params.workspace_name or ticket_workspace_name(ticket, params.ticket_id),
    }
    local first_step = repo.next_step(run)
    local activation = M.activate_step(run, first_step)
    return { run = repo.get_run(run.id), activation = activation }
end

function M.request_step_advance(params, context)
    local run_id = params.run_id
    if util.is_blank(run_id) and context and context.session_uuid then
        local assignment = repo.find_active_assignment(context.session_uuid)
        run_id = assignment and assignment.run_id or nil
    end
    util.assert_present(run_id, "run_id")

    local run = repo.get_run(run_id)
    if not run then
        error("run not found: " .. tostring(run_id))
    end
    if util.is_blank(run.current_step_id) then
        return { ok = false, status = run.status, error = "run has no current step" }
    end
    local step = repo.get_step(run.current_step_id)
    local active_visit_id = run.current_run_step_id
    local missing = has_review_kickback(run, step) and {} or unmet_gates(run, step)
    if #missing > 0 then
        repo.append_event("step.advance_blocked", {
            run_id = run.id,
            ticket_id = run.ticket_id,
            payload = { step_id = step.id, unmet_gates = missing, summary = params.summary },
        })
        return { ok = false, status = "blocked", step = step, unmet_gates = missing }
    end

    local next_step, transition_error = repo.next_step(run, step, active_visit_id)
    if transition_error then
        repo.update_run(run.id, { status = "blocked" })
        if active_visit_id then
            repo.update_run_step_visit(active_visit_id, { status = "blocked" })
        end
        repo.append_event("step.advance_blocked", {
            run_id = run.id,
            ticket_id = run.ticket_id,
            payload = { step_id = step.id, run_step_id = active_visit_id, transition_error = transition_error },
        })
        return { ok = false, status = "blocked", step = step, transition_error = transition_error }
    end
    if active_visit_id then
        repo.update_run_step_visit(active_visit_id, { status = "done", completed_at = util.now() })
    else
        repo.update_run_step(run.id, step.id, { status = "done", completed_at = util.now() })
    end
    repo.append_event("step.completed", {
        run_id = run.id,
        ticket_id = run.ticket_id,
        payload = { step_id = step.id, run_step_id = active_visit_id, summary = params.summary, evidence = params.evidence or {} },
    })
    local updated = repo.get_run(run.id)
    local activation = M.activate_step(updated, next_step)
    return { ok = true, completed_step = step, next_step = next_step, activation = activation, run = repo.get_run(run.id) }
end

function M.handle_command_gate_completed(data)
    local context = data and data.context or {}
    if context.owner_plugin ~= OWNER then
        return
    end
    local run_id = context.run_id
    local step_id = context.step_id
    if util.is_blank(run_id) or util.is_blank(step_id) then
        return
    end
    local gate_id = context.gate_id
    if util.is_blank(gate_id) then
        local gates = repo.step_gates(step_id)
        gate_id = gates[1] and gates[1].id or step_id
    end
    local success = data.success == true
    local status = success and "passed" or "failed"
    local summary = success and "Command gate passed" or ("Command gate failed" .. (data.exit_status and (" with exit status " .. tostring(data.exit_status)) or ""))
    if data.error and data.error ~= "" then
        summary = summary .. ": " .. tostring(data.error)
    end
    repo.submit_gate{
        run_id = run_id,
        run_step_id = context.run_step_id,
        step_id = step_id,
        gate_id = gate_id,
        status = status,
        summary = data.summary or summary,
        evidence = data,
    }
    if status == "passed" then
        pcall(function()
            M.request_step_advance({ run_id = run_id, summary = "Command gate passed", evidence = data }, {})
        end)
    else
        repo.update_run(run_id, { status = "blocked" })
        if not util.is_blank(context.run_step_id) then
            repo.update_run_step_visit(context.run_step_id, { status = "blocked" })
        else
            repo.update_run_step(run_id, step_id, { status = "blocked" })
        end
    end
end

function M.handle_agent_created(info)
    local metadata = info and info.metadata or {}
    local request_id = info and (info.request_id or metadata.request_id)
    if util.is_blank(request_id) then
        return
    end

    local ticket_id, question_id = tostring(request_id):match("^" .. lua_pattern_escape(OWNER) .. ":(.-):question:(.-):agent$")
    if not util.is_blank(question_id) then
        local session_uuid = info.session_uuid or info.uuid or info.id
        if not util.is_blank(session_uuid) then
            pcall(repo.update_question, question_id, { advisor_session_uuid = session_uuid })
            repo.append_event("question.agent_linked", {
                ticket_id = ticket_id,
                payload = { question_id = question_id, session_uuid = session_uuid, request_id = request_id },
            })
            refresh_surfaces()
        end
        return
    end

    local merge_ticket_id = tostring(request_id):match("^" .. lua_pattern_escape(OWNER) .. ":(.-):merge:agent$")
    if not util.is_blank(merge_ticket_id) then
        local session_uuid = info.session_uuid or info.uuid or info.id
        if not util.is_blank(session_uuid) then
            repo.append_event("ticket.merge_agent_linked", {
                ticket_id = merge_ticket_id,
                payload = { session_uuid = session_uuid, request_id = request_id },
            })
            refresh_surfaces()
        end
        return
    end

    local run_id, step_id = tostring(request_id):match("^" .. lua_pattern_escape(OWNER) .. ":(.-):(.-):agent$")
    if util.is_blank(run_id) or util.is_blank(step_id) then
        return
    end

    local session_uuid = info.session_uuid or info.uuid or info.id
    if util.is_blank(session_uuid) then
        return
    end

    local run = repo.get_run(run_id)
    if not run then
        return
    end

    local run_step = nil
    if run.current_run_step_id then
        run_step = repo.get_run_step_visit(run.current_run_step_id)
    end
    if not run_step or run_step.step_id ~= step_id then
        run_step = repo.get_run_step(run_id, step_id)
    end
    if not run_step then
        return
    end

    if run_step.id then
        repo.update_run_step_visit(run_step.id, { agent_session_uuid = session_uuid })
    else
        repo.update_run_step(run.id, step_id, { agent_session_uuid = session_uuid })
    end
    repo.append_event("step.agent_linked", {
        run_id = run.id,
        ticket_id = run.ticket_id,
        payload = {
            step_id = step_id,
            run_step_id = run_step.id,
            session_uuid = session_uuid,
            request_id = request_id,
        },
    })
    refresh_surfaces()
end

function M.handle_agent_lifecycle(info)
    local metadata = info and info.metadata or {}
    local request_id = info and (info.request_id or metadata.request_id)
    if util.is_blank(request_id) then
        return
    end

    local run_id, step_id = tostring(request_id):match("^" .. lua_pattern_escape(OWNER) .. ":(.-):(.-):agent$")
    if util.is_blank(run_id) or util.is_blank(step_id) then
        return
    end

    local run = repo.get_run(run_id)
    if not run then
        return
    end

    repo.append_event("step.agent_lifecycle", {
        run_id = run.id,
        ticket_id = run.ticket_id,
        payload = {
            step_id = step_id,
            request_id = request_id,
            status = info.status,
            error = info.error,
        },
    })

    if info.status == "failed" then
        repo.update_run(run.id, { status = "blocked" })
        if run.current_run_step_id then
            repo.update_run_step_visit(run.current_run_step_id, { status = "blocked" })
        else
            repo.update_run_step(run.id, step_id, { status = "blocked" })
        end
    end
    refresh_surfaces()
end

function M.reconcile_agent_sessions()
    local ok, HubModule = pcall(require, "lib.hub")
    if not ok or not HubModule or not HubModule.get then
        return
    end
    local hub = HubModule.get()
    if not hub or type(hub.list_owned_sessions) ~= "function" then
        return
    end
    local sessions = hub:list_owned_sessions(OWNER) or {}
    for _, session in ipairs(sessions) do
        M.handle_agent_created(session)
    end
end

function M.refresh_surfaces(ctx)
    refresh_surfaces(ctx)
end

return M
