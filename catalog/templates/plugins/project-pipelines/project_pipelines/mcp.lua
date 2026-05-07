-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/mcp.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local engine = require("project_pipelines.engine")
local ConfigResolver = require("lib.config_resolver")

local M = {}

local finding_schema = {
    type = "object",
    properties = {
        severity = { type = "string", enum = { "blocker", "high", "medium", "low", "info" } },
        title = { type = "string" },
        file = { type = "string" },
        line = { type = "integer" },
        details = { type = "string" },
        suggested_fix = { type = "string" },
    },
    required = { "title" },
}

local gate_schema = {
    type = "object",
    properties = {
        id = { type = "string" },
        kind = { type = "string", enum = { "attestation", "review_clear", "command" } },
        prompt = { type = "string" },
        required_fields = { type = "array", items = { type = "string" } },
        command = { type = "string" },
    },
    required = { "prompt" },
}

local step_schema = {
    type = "object",
    properties = {
        id = { type = "string" },
        name = { type = "string" },
        position = { type = "integer" },
        kind = { type = "string", enum = { "agent", "command" } },
        agent_name = { type = "string" },
        prompt = { type = "string" },
        command = { type = "string" },
        next_step_id = { type = "string" },
        on_approved_step_id = { type = "string" },
        on_changes_requested_step_id = { type = "string" },
        on_blocked_step_id = { type = "string" },
        gates = { type = "array", items = gate_schema },
    },
    required = { "name" },
}

local function ok(value)
    return { ok = true, result = value }
end

local function sync_ok(value)
    return ok(value)
end

local function tool(name, spec, handler)
    mcp.tool(name, spec, function(params, context)
        return handler(params or {}, context or {})
    end)
end

local function list_agent_choices(target_path)
    local choices = {}
    local root = nil
    if config and config.data_dir then
        local ok, data_dir = pcall(config.data_dir)
        if ok then
            root = data_dir
        end
    end
    for _, name in ipairs(ConfigResolver.list_agents(root, target_path)) do
        table.insert(choices, { name = name, label = name })
    end
    if #choices == 0 then
        return {
            { name = "codex", label = "codex" },
            { name = "claude", label = "claude" },
        }
    end
    table.sort(choices, function(a, b) return a.name < b.name end)
    return choices
end

function M.register()
    repo.prune_legacy_seed_data()

    if mcp.prompt then
        mcp.prompt("project-pipelines-agent-role", {
            description = "System-level instructions for agents running a Project Pipelines step.",
            arguments = {
                { name = "role", description = "Pipeline role or step name", required = false },
            },
        }, function(args)
            local role = args and args.role or "pipeline step"
            return "You are operating as a Project Pipelines " .. role .. ". First call project_pipelines_current_context. Treat returned gate prompts as hard requirements. State assumptions explicitly, prefer surgical changes, avoid speculative abstractions, and define verifiable success criteria. Submit gate evidence, reviews, findings, and artifacts through the project_pipelines_* MCP tools. Request advancement only after required evidence proves the actual production/user/runtime path changed. Do not leave dead, deprecated, or unwired code behind."
        end)

        mcp.prompt("project-pipelines-review-role", {
            description = "System-level instructions for Project Pipelines review agents.",
            arguments = {},
        }, function(_args)
            return "You are a Project Pipelines review agent. Review for correctness, regressions, architecture fit, missing tests, documentation gaps, hidden assumptions, overcomplication, speculative scope, dead code, deprecated code paths, and unwired implementation. Everything claimed complete must be wired into the product and covered by focused verification of the actual production/user/runtime path. Do not accept pre-existing failures as a blanket excuse; require the agent to either fix them or prove with exact evidence that they are unrelated to this ticket. Submit project_pipelines_submit_review with verdict, summary, and structured findings. Blocker and high findings block review_clear gates until resolved or waived."
        end)
    end

    tool("project_pipelines_create_ticket", {
        description = "Create a project pipeline ticket.",
        input_schema = {
            type = "object",
            properties = {
                title = { type = "string" },
                description = { type = "string" },
                project_id = { type = "string" },
                target_id = { type = "string" },
                target_path = { type = "string" },
            },
            required = { "title", "target_id" },
        },
    }, function(params)
        return sync_ok(repo.create_ticket(params))
    end)

    tool("project_pipelines_list_tickets", {
        description = "List project pipeline tickets.",
        input_schema = { type = "object", properties = {} },
    }, function()
        return ok(repo.list_tickets())
    end)

    tool("project_pipelines_search_tickets", {
        description = "Search tickets by text, status, project, target, and whether closed tickets should be included.",
        input_schema = {
            type = "object",
            properties = {
                query = { type = "string" },
                status = { type = "string", enum = { "open", "active", "blocked", "closed" } },
                project_id = { type = "string" },
                target_id = { type = "string" },
                include_closed = { type = "boolean" },
                limit = { type = "integer" },
            },
        },
    }, function(params)
        return ok(repo.search_tickets(params or {}))
    end)

    tool("project_pipelines_get_ticket", {
        description = "Get one ticket with its project, runs, current status, run steps, sessions, and open findings.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
            },
            required = { "ticket_id" },
        },
    }, function(params)
        return ok(repo.ticket_status(params.ticket_id))
    end)

    tool("project_pipelines_update_ticket", {
        description = "Update a ticket's title, description, project, target, or status.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                title = { type = "string" },
                description = { type = "string" },
                project_id = { type = "string" },
                target_id = { type = "string" },
                target_path = { type = "string" },
                status = { type = "string", enum = { "open", "active", "blocked", "closed" } },
            },
            required = { "ticket_id" },
        },
    }, function(params)
        local fields = {}
        for _, field in ipairs({ "title", "description", "project_id", "target_id", "target_path", "status" }) do
            if params[field] ~= nil then fields[field] = params[field] end
        end
        return sync_ok(repo.update_ticket(params.ticket_id, fields))
    end)

    tool("project_pipelines_delete_ticket", {
        description = "Delete a ticket that has no run history.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
            },
            required = { "ticket_id" },
        },
    }, function(params)
        return sync_ok(repo.delete_ticket(params.ticket_id))
    end)

    tool("project_pipelines_create_project", {
        description = "Create an optional project for multi-phase or coordinated work.",
        input_schema = {
            type = "object",
            properties = {
                name = { type = "string" },
                description = { type = "string" },
                target_id = { type = "string" },
                target_path = { type = "string" },
            },
            required = { "name" },
        },
    }, function(params)
        return sync_ok(repo.create_project(params))
    end)

    tool("project_pipelines_list_projects", {
        description = "List project pipeline projects.",
        input_schema = { type = "object", properties = {} },
    }, function()
        return ok(repo.list_projects())
    end)

    tool("project_pipelines_get_project", {
        description = "Get one project with its tickets and spawn targets.",
        input_schema = {
            type = "object",
            properties = {
                project_id = { type = "string" },
            },
            required = { "project_id" },
        },
    }, function(params)
        return ok(repo.project_detail(params.project_id))
    end)

    tool("project_pipelines_update_project", {
        description = "Update a project's name, description, or status.",
        input_schema = {
            type = "object",
            properties = {
                project_id = { type = "string" },
                name = { type = "string" },
                description = { type = "string" },
                status = { type = "string", enum = { "open", "active", "blocked", "closed" } },
            },
            required = { "project_id" },
        },
    }, function(params)
        local fields = {}
        for _, field in ipairs({ "name", "description", "status" }) do
            if params[field] ~= nil then fields[field] = params[field] end
        end
        return sync_ok(repo.update_project(params.project_id, fields))
    end)

    tool("project_pipelines_delete_project", {
        description = "Delete a project that has no tickets. Project spawn-target rows are deleted with it.",
        input_schema = {
            type = "object",
            properties = {
                project_id = { type = "string" },
            },
            required = { "project_id" },
        },
    }, function(params)
        return sync_ok(repo.delete_project(params.project_id))
    end)

    tool("project_pipelines_add_project_target", {
        description = "Attach a spawn target to a project.",
        input_schema = {
            type = "object",
            properties = {
                project_id = { type = "string" },
                target_id = { type = "string" },
                target_path = { type = "string" },
            },
            required = { "project_id", "target_id" },
        },
    }, function(params)
        return sync_ok(repo.add_project_target(params.project_id, params.target_id, params.target_path))
    end)

    tool("project_pipelines_remove_project_target", {
        description = "Remove one spawn target row from a project.",
        input_schema = {
            type = "object",
            properties = {
                project_target_id = { type = "string" },
            },
            required = { "project_target_id" },
        },
    }, function(params)
        return sync_ok(repo.remove_project_target(params.project_target_id))
    end)

    tool("project_pipelines_list_pipelines", {
        description = "List available project pipelines with ordered steps and gate prompts.",
        input_schema = { type = "object", properties = {} },
    }, function()
        local pipelines = repo.list_pipelines()
        for _, pipeline in ipairs(pipelines) do
            local definition = repo.get_pipeline_definition(pipeline.id)
            pipeline.steps = definition and definition.steps or {}
        end
        return ok(pipelines)
    end)

    tool("project_pipelines_get_pipeline", {
        description = "Get one project pipeline definition with ordered steps and gates.",
        input_schema = {
            type = "object",
            properties = {
                pipeline_id = { type = "string" },
            },
            required = { "pipeline_id" },
        },
    }, function(params)
        return ok(repo.get_pipeline_definition(params.pipeline_id))
    end)

    tool("project_pipelines_create_pipeline", {
        description = "Create a project pipeline definition. Agents use this to define reusable ticket pipelines explicitly; Botster does not seed default pipelines.",
        input_schema = {
            type = "object",
            properties = {
                id = { type = "string" },
                name = { type = "string" },
                description = { type = "string" },
                merge_policy = { type = "string", enum = { "direct", "pr" } },
                steps = { type = "array", items = step_schema },
            },
            required = { "id", "name", "steps" },
        },
    }, function(params)
        return sync_ok(repo.create_pipeline(params))
    end)

    tool("project_pipelines_update_pipeline", {
        description = "Update a pipeline definition's name, description, or merge policy.",
        input_schema = {
            type = "object",
            properties = {
                pipeline_id = { type = "string" },
                name = { type = "string" },
                description = { type = "string" },
                merge_policy = { type = "string", enum = { "direct", "pr" } },
            },
            required = { "pipeline_id" },
        },
    }, function(params)
        local fields = {}
        if params.name ~= nil then fields.name = params.name end
        if params.description ~= nil then fields.description = params.description end
        if params.merge_policy ~= nil then fields.merge_policy = params.merge_policy end
        return sync_ok(repo.update_pipeline(params.pipeline_id, fields))
    end)

    tool("project_pipelines_delete_pipeline", {
        description = "Delete a pipeline definition that has no run history. Steps and gates are deleted with it.",
        input_schema = {
            type = "object",
            properties = {
                pipeline_id = { type = "string" },
            },
            required = { "pipeline_id" },
        },
    }, function(params)
        return sync_ok(repo.delete_pipeline(params.pipeline_id))
    end)

    tool("project_pipelines_create_step", {
        description = "Create a step in an existing pipeline definition.",
        input_schema = {
            type = "object",
            properties = {
                pipeline_id = { type = "string" },
                id = { type = "string" },
                name = { type = "string" },
                position = { type = "integer" },
                kind = { type = "string", enum = { "agent", "command" } },
                agent_name = { type = "string" },
                prompt = { type = "string" },
                command = { type = "string" },
                next_step_id = { type = "string" },
                on_approved_step_id = { type = "string" },
                on_changes_requested_step_id = { type = "string" },
                on_blocked_step_id = { type = "string" },
                gates = { type = "array", items = gate_schema },
            },
            required = { "pipeline_id", "name" },
        },
    }, function(params)
        return sync_ok(repo.create_step(params))
    end)

    tool("project_pipelines_update_step", {
        description = "Update a pipeline step definition.",
        input_schema = {
            type = "object",
            properties = {
                step_id = { type = "string" },
                name = { type = "string" },
                position = { type = "integer" },
                kind = { type = "string", enum = { "agent", "command" } },
                agent_name = { type = "string" },
                prompt = { type = "string" },
                command = { type = "string" },
                next_step_id = { type = "string" },
                on_approved_step_id = { type = "string" },
                on_changes_requested_step_id = { type = "string" },
                on_blocked_step_id = { type = "string" },
            },
            required = { "step_id" },
        },
    }, function(params)
        local fields = {}
        for _, field in ipairs({ "name", "position", "kind", "agent_name", "prompt", "command", "next_step_id", "on_approved_step_id", "on_changes_requested_step_id", "on_blocked_step_id" }) do
            if params[field] ~= nil then fields[field] = params[field] end
        end
        return sync_ok(repo.update_step(params.step_id, fields))
    end)

    tool("project_pipelines_delete_step", {
        description = "Delete a pipeline step that has no run history. Gates under the step are deleted with it.",
        input_schema = {
            type = "object",
            properties = {
                step_id = { type = "string" },
            },
            required = { "step_id" },
        },
    }, function(params)
        return sync_ok(repo.delete_step(params.step_id))
    end)

    tool("project_pipelines_create_gate", {
        description = "Create a gate under an existing pipeline step.",
        input_schema = {
            type = "object",
            properties = {
                step_id = { type = "string" },
                id = { type = "string" },
                kind = { type = "string", enum = { "attestation", "review_clear", "command" } },
                prompt = { type = "string" },
                required_fields = { type = "array", items = { type = "string" } },
                command = { type = "string" },
            },
            required = { "step_id", "prompt" },
        },
    }, function(params)
        return sync_ok(repo.create_gate(params))
    end)

    tool("project_pipelines_update_gate", {
        description = "Update a pipeline gate definition.",
        input_schema = {
            type = "object",
            properties = {
                gate_id = { type = "string" },
                kind = { type = "string", enum = { "attestation", "review_clear", "command" } },
                prompt = { type = "string" },
                required_fields = { type = "array", items = { type = "string" } },
                command = { type = "string" },
            },
            required = { "gate_id" },
        },
    }, function(params)
        local fields = {}
        for _, field in ipairs({ "kind", "prompt", "required_fields", "command" }) do
            if params[field] ~= nil then fields[field] = params[field] end
        end
        return sync_ok(repo.update_gate(params.gate_id, fields))
    end)

    tool("project_pipelines_delete_gate", {
        description = "Delete a pipeline gate that has no submitted gate results.",
        input_schema = {
            type = "object",
            properties = {
                gate_id = { type = "string" },
            },
            required = { "gate_id" },
        },
    }, function(params)
        return sync_ok(repo.delete_gate(params.gate_id))
    end)

    tool("project_pipelines_list_agent_choices", {
        description = "List available Botster agent definitions for assigning pipeline steps.",
        input_schema = {
            type = "object",
            properties = {
                target_path = { type = "string" },
            },
        },
    }, function(params)
        return ok(list_agent_choices(params.target_path))
    end)

    tool("project_pipelines_update_step_agent", {
        description = "Set the Botster agent definition used by one pipeline step.",
        input_schema = {
            type = "object",
            properties = {
                step_id = { type = "string" },
                agent_name = { type = "string" },
            },
            required = { "step_id", "agent_name" },
        },
    }, function(params)
        return sync_ok(repo.update_step_agent(params.step_id, params.agent_name))
    end)

    tool("project_pipelines_start_run", {
        description = "Start a pipeline run for a ticket. Agent steps require target_id or target_path; command steps require target_path.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                pipeline_id = { type = "string" },
                parent_run_id = { type = "string" },
                target_id = { type = "string" },
                target_path = { type = "string" },
                workspace_id = { type = "string" },
                workspace_name = { type = "string" },
            },
            required = { "ticket_id" },
        },
    }, function(params)
        return sync_ok(engine.start_run(params))
    end)

    tool("project_pipelines_request_merge", {
        description = "Spawn a merge agent for a ticket whose latest run is complete.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                agent_name = { type = "string" },
                workspace_name = { type = "string" },
                strategy = { type = "string" },
            },
            required = { "ticket_id" },
        },
    }, function(params, context)
        return ok(engine.request_merge(params, context))
    end)

    tool("project_pipelines_close_ticket", {
        description = "Close a ticket. Completed pipeline work requires merge_confirmed=true and should be called by the merge agent only after merge or PR completion. When merge_confirmed is true, include merge_commit, pr_url, or merge_summary when available so the ticket keeps a merge artifact.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                merge_confirmed = { type = "boolean" },
                merge_commit = { type = "string" },
                pr_url = { type = "string" },
                merge_summary = { type = "string" },
            },
            required = { "ticket_id" },
        },
    }, function(params, context)
        return ok(engine.close_ticket(params.ticket_id, {
            merge_confirmed = params.merge_confirmed == true,
            merge_commit = params.merge_commit,
            pr_url = params.pr_url,
            merge_summary = params.merge_summary,
            closed_by_session_uuid = context and context.session_uuid,
        }))
    end)

    tool("project_pipelines_add_ticket_dependency", {
        description = "Add an ordering dependency. The ticket cannot start a pipeline run until the dependency ticket is closed.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                depends_on_ticket_id = { type = "string" },
            },
            required = { "ticket_id", "depends_on_ticket_id" },
        },
    }, function(params)
        return sync_ok(repo.add_ticket_dependency(params.ticket_id, params.depends_on_ticket_id))
    end)

    tool("project_pipelines_remove_ticket_dependency", {
        description = "Remove a ticket ordering dependency.",
        input_schema = {
            type = "object",
            properties = {
                dependency_id = { type = "string" },
            },
            required = { "dependency_id" },
        },
    }, function(params)
        return sync_ok(repo.remove_ticket_dependency(params.dependency_id))
    end)

    tool("project_pipelines_list_ticket_dependencies", {
        description = "List ordering dependencies for a ticket, including whether dependency tickets are still open.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                blocking_only = { type = "boolean" },
            },
            required = { "ticket_id" },
        },
    }, function(params)
        if params.blocking_only then
            return ok(repo.blocking_ticket_dependencies(params.ticket_id))
        end
        return ok(repo.ticket_dependencies(params.ticket_id))
    end)

    tool("project_pipelines_current_context", {
        description = "Return ticket, run, current step, gate prompts, reviews, findings, artifacts, dependencies, questions, and events for the current pipeline run. If run_id is omitted, infer it from the calling agent session.",
        input_schema = {
            type = "object",
            properties = {
                run_id = { type = "string" },
            },
        },
    }, function(params, context)
        return ok(engine.context_for(params.run_id, context.session_uuid))
    end)

    tool("project_pipelines_ask_human", {
        description = "Ask the human a durable pipeline question visible in the Project Pipelines sidebar.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                run_id = { type = "string" },
                question = { type = "string" },
                blocking = { type = "boolean" },
            },
            required = { "question" },
        },
    }, function(params, context)
        return ok(engine.ask_human(params, context))
    end)

    tool("project_pipelines_ask_agent", {
        description = "Ask a new or configured advisor agent a durable pipeline question.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                run_id = { type = "string" },
                question = { type = "string" },
                blocking = { type = "boolean" },
                agent_name = { type = "string" },
                workspace_name = { type = "string" },
            },
            required = { "question" },
        },
    }, function(params, context)
        return ok(engine.ask_agent(params, context))
    end)

    tool("project_pipelines_answer_question", {
        description = "Answer a Project Pipelines question and notify the asking session to read the durable answer with project_pipelines_receive_question_answers.",
        input_schema = {
            type = "object",
            properties = {
                question_id = { type = "string" },
                answer = { type = "string" },
                status = { type = "string", enum = { "answered", "dismissed" } },
            },
            required = { "question_id", "answer" },
        },
    }, function(params, context)
        return ok(engine.answer_question(params, context))
    end)

    tool("project_pipelines_receive_question_answers", {
        description = "Return durable answers for Project Pipelines questions asked by the calling session, optionally filtered by ticket, run, question, or status.",
        input_schema = {
            type = "object",
            properties = {
                ticket_id = { type = "string" },
                run_id = { type = "string" },
                question_id = { type = "string" },
                status = { type = "string", enum = { "answered", "dismissed" } },
                all = { type = "boolean" },
            },
        },
    }, function(params, context)
        return ok(engine.question_answers(params, context))
    end)

    tool("project_pipelines_submit_gate", {
        description = "Submit evidence for a pipeline gate. Agents should submit required gate evidence before requesting advancement.",
        input_schema = {
            type = "object",
            properties = {
                run_id = { type = "string" },
                run_step_id = { type = "string" },
                step_id = { type = "string" },
                gate_id = { type = "string" },
                status = { type = "string", enum = { "passed", "failed", "waived" } },
                summary = { type = "string" },
                evidence = { type = "object" },
            },
            required = { "run_id", "step_id", "gate_id" },
        },
    }, function(params, context)
        params.created_by_session_uuid = context.session_uuid
        return sync_ok(repo.submit_gate(params))
    end)

    tool("project_pipelines_request_step_advance", {
        description = "Ask the pipeline engine to move the current step forward. Returns unmet gate prompts when advancement is blocked.",
        input_schema = {
            type = "object",
            properties = {
                run_id = { type = "string" },
                summary = { type = "string" },
                evidence = { type = "object" },
            },
        },
    }, function(params, context)
        return sync_ok(engine.request_step_advance(params, context))
    end)

    tool("project_pipelines_submit_review", {
        description = "Submit a structured review for a pipeline step, including findings that become visible to every agent in the run context.",
        input_schema = {
            type = "object",
            properties = {
                run_id = { type = "string" },
                run_step_id = { type = "string" },
                step_id = { type = "string" },
                verdict = { type = "string", enum = { "approved", "changes_required", "blocked" } },
                summary = { type = "string" },
                findings = { type = "array", items = finding_schema },
            },
            required = { "run_id", "step_id", "verdict" },
        },
    }, function(params, context)
        params.reviewer_session_uuid = context.session_uuid
        return sync_ok(repo.create_review(params))
    end)

    tool("project_pipelines_resolve_finding", {
        description = "Mark a review finding resolved or waived with a resolution note.",
        input_schema = {
            type = "object",
            properties = {
                finding_id = { type = "string" },
                status = { type = "string", enum = { "resolved", "waived" } },
                resolution = { type = "string" },
            },
            required = { "finding_id", "resolution" },
        },
    }, function(params)
        return sync_ok(repo.resolve_finding(params.finding_id, params))
    end)

    tool("project_pipelines_add_artifact", {
        description = "Attach a durable artifact to a run, such as a plan, patch summary, command result, or external URL.",
        input_schema = {
            type = "object",
            properties = {
                run_id = { type = "string" },
                run_step_id = { type = "string" },
                step_id = { type = "string" },
                kind = { type = "string" },
                uri = { type = "string" },
                summary = { type = "string" },
                payload = { type = "object" },
            },
            required = { "run_id" },
        },
    }, function(params)
        return sync_ok(repo.add_artifact(params))
    end)

    tool("project_pipelines_create_child_run", {
        description = "Create a child ticket and pipeline run for a slice of a larger parent run.",
        input_schema = {
            type = "object",
            properties = {
                parent_run_id = { type = "string" },
                title = { type = "string" },
                description = { type = "string" },
                pipeline_id = { type = "string" },
                target_id = { type = "string" },
                target_path = { type = "string" },
                workspace_id = { type = "string" },
                workspace_name = { type = "string" },
            },
            required = { "parent_run_id", "title" },
        },
    }, function(params)
        local parent = repo.get_run(params.parent_run_id)
        if not parent then
            error("parent_run_id not found")
        end
        local target_id = params.target_id or parent.target_id
        local target_path = params.target_path or parent.target_path
        local ticket = repo.create_ticket{
            title = params.title,
            description = params.description or "",
            target_id = target_id,
            target_path = target_path,
        }
        params.ticket_id = ticket.id
        params.target_id = target_id
        params.target_path = target_path
        params.workspace_id = params.workspace_id or parent.workspace_id
        params.workspace_name = params.workspace_name or parent.workspace_name
        local started = engine.start_run(params)
        repo.append_event("run.child_created", {
            run_id = parent.id,
            ticket_id = parent.ticket_id,
            payload = { child_run_id = started.run.id, child_ticket_id = ticket.id },
        })
        return sync_ok({ ticket = ticket, run = started.run, activation = started.activation })
    end)
end

return M
