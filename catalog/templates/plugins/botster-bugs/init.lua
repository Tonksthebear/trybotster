-- @template Botster Bugs
-- @description Route Botster bug reports to a live Codex orchestrator that files Project Pipelines tickets
-- @category plugins
-- @dest plugins/botster-bugs/init.lua
-- @scope device
-- @version 1.0.0

-- Botster Bugs plugin
--
-- Live MCP ingress for Botster bug reports. This plugin intentionally keeps no
-- durable records; the orchestrator agent makes reports durable by creating
-- Project Pipelines tickets and runs.

local Agent = require("lib.agent")
local Hub = require("lib.hub")

local OWNER = "botster-bugs"
local WORKSPACE_NAME = "Botster Bugs"
local ORCHESTRATOR_LABEL = "Botster Bugs Orchestrator"
local DEFAULT_AGENT_NAME = "codex"

local function is_blank(value)
    return value == nil or tostring(value):match("^%s*$") ~= nil
end

local function trim(value)
    return tostring(value or ""):gsub("^%s+", ""):gsub("%s+$", "")
end

local function first_nonblank(...)
    for i = 1, select("#", ...) do
        local value = select(i, ...)
        if not is_blank(value) then return value end
    end
    return nil
end

local function table_to_lines(values)
    local lines = {}
    for _, value in ipairs(values or {}) do
        if not is_blank(value) then lines[#lines + 1] = "- " .. trim(value) end
    end
    return table.concat(lines, "\n")
end

local function current_caller(context)
    local session_uuid = context and context.session_uuid
    if is_blank(session_uuid) then return nil end
    local agent = Agent.get(session_uuid)
    if not agent or type(agent.info) ~= "function" then return nil end
    local ok, info = pcall(function() return agent:info() end)
    if ok then return info end
    return nil
end

local function target_from_name(name)
    if is_blank(name) or not spawn_targets or type(spawn_targets.list) ~= "function" then
        return nil
    end
    for _, target in ipairs(spawn_targets.list() or {}) do
        if target.name == name then return target end
    end
    return nil
end

local function target_from_id(id)
    if is_blank(id) then return nil end
    if spawn_targets and type(spawn_targets.get) == "function" then
        local target = spawn_targets.get(id)
        if target then return target end
    end
    return { id = id }
end

local function inspect_target(target)
    if not target or not target.path or not spawn_targets or type(spawn_targets.inspect) ~= "function" then
        return nil
    end
    local ok, inspection = pcall(function() return spawn_targets.inspect(target.path) end)
    if ok then return inspection end
    return nil
end

local function resolve_target(params, caller)
    local target = target_from_id(params.target_id)
        or target_from_name(params.target_name)

    if not target then
        target = target_from_name("trybotster")
    end

    if not target and caller and not is_blank(caller.target_id) then
        target = target_from_id(caller.target_id)
    end

    if not target then
        return nil, "target_id is required because no caller target or trybotster spawn target could be resolved"
    end

    local inspection = inspect_target(target)
    return {
        target_id = target.id,
        target_path = target.path,
        target_repo = target.repo_name or (inspection and inspection.repo_name) or nil,
    }, nil
end

local function live_orchestrator(target)
    local sessions = Hub.get():list_owned_sessions(OWNER) or {}
    for _, session in ipairs(sessions) do
        local metadata = session.metadata or {}
        local session_target_id = session.target_id or metadata.target_id
        if metadata.role == "orchestrator"
            and session.session_uuid
            and session.status ~= "closed"
            and (not target or is_blank(target.target_id) or session_target_id == target.target_id) then
            return session
        end
    end
    return nil
end

local function report_payload(params, context, caller, target)
    return {
        title = trim(params.title),
        severity = first_nonblank(params.severity, "bug"),
        description = params.description,
        reproduction_steps = params.reproduction_steps,
        expected = params.expected,
        actual = params.actual,
        evidence = params.evidence,
        source = params.source,
        caller = {
            session_uuid = context and context.session_uuid or nil,
            hub_id = context and context.hub_id or nil,
            label = caller and caller.label or nil,
            agent_name = caller and caller.agent_name or nil,
            workspace_name = caller and caller.workspace_name or nil,
            workspace_id = caller and caller.workspace_id or nil,
            target_id = caller and caller.target_id or nil,
            worktree_path = caller and caller.worktree_path or nil,
            branch_name = caller and caller.branch_name or nil,
        },
        target = target,
        filed_at = os.time(),
    }
end

local function render_report(report)
    local sections = {
        "# Botster Bug Report",
        "",
        "Title: " .. tostring(report.title),
        "Severity: " .. tostring(report.severity),
    }

    if not is_blank(report.description) then
        sections[#sections + 1] = ""
        sections[#sections + 1] = "Description:"
        sections[#sections + 1] = trim(report.description)
    end

    local steps = table_to_lines(report.reproduction_steps)
    if not is_blank(steps) then
        sections[#sections + 1] = ""
        sections[#sections + 1] = "Reproduction steps:"
        sections[#sections + 1] = steps
    end

    for _, pair in ipairs({
        { "Expected", report.expected },
        { "Actual", report.actual },
        { "Evidence", report.evidence },
        { "Source", report.source },
    }) do
        if not is_blank(pair[2]) then
            sections[#sections + 1] = ""
            sections[#sections + 1] = pair[1] .. ":"
            sections[#sections + 1] = trim(pair[2])
        end
    end

    if report.caller and not is_blank(report.caller.session_uuid) then
        sections[#sections + 1] = ""
        sections[#sections + 1] = "Filed by:"
        sections[#sections + 1] = "- session_uuid: " .. tostring(report.caller.session_uuid)
        if not is_blank(report.caller.label) then sections[#sections + 1] = "- label: " .. tostring(report.caller.label) end
        if not is_blank(report.caller.workspace_name) then sections[#sections + 1] = "- workspace: " .. tostring(report.caller.workspace_name) end
        if not is_blank(report.caller.worktree_path) then sections[#sections + 1] = "- worktree_path: " .. tostring(report.caller.worktree_path) end
    end

    if report.target and not is_blank(report.target.target_id) then
        sections[#sections + 1] = ""
        sections[#sections + 1] = "Target:"
        sections[#sections + 1] = "- target_id: " .. tostring(report.target.target_id)
        if not is_blank(report.target.target_path) then sections[#sections + 1] = "- target_path: " .. tostring(report.target.target_path) end
        if not is_blank(report.target.target_repo) then sections[#sections + 1] = "- target_repo: " .. tostring(report.target.target_repo) end
    end

    return table.concat(sections, "\n")
end

local function orchestrator_prompt(report)
    return table.concat({
        "You are the Botster Bugs orchestrator agent.",
        "",
        "Your job is to turn incoming Botster bug reports into durable Project Pipelines tickets, then orchestrate fixes through the pipeline plugin.",
        "",
        "Rules:",
        "- Make each actionable bug durable by calling project_pipelines_create_ticket with the target_id from the report.",
        "- If an appropriate project or pipeline exists, use Project Pipelines tools to attach the ticket and start or route the run.",
        "- Delegate implementation, verification, review, and follow-up through Project Pipelines agents and Botster MCP messaging.",
        "- Keep the bug-report plugin stateless; do not rely on this initial prompt or inbox message as the durable record.",
        "- If the report is ambiguous, create a ticket that captures the ambiguity and ask focused follow-up through Project Pipelines.",
        "",
        "Initial report:",
        render_report(report),
    }, "\n")
end

local function post_to_orchestrator(session_uuid, report, context)
    local payload = {
        kind = "botster_bug_report",
        report = report,
        instructions = table.concat({
            "Create or update the durable Project Pipelines ticket for this Botster bug.",
            "Use project_pipelines_create_ticket with report.target.target_id for new actionable bugs.",
            "Then route/delegate work using Project Pipelines tools.",
        }, " "),
        rendered = render_report(report),
    }

    local result = Hub.get():post(session_uuid, {
        type = "task",
        payload = payload,
        expires_in = 86400,
        from_agent_id = context and context.session_uuid or "botster-bugs",
        from_label = "botster-bugs",
    })

    pcall(function()
        Hub.get():notify(session_uuid, {
            source = OWNER,
            title = "New Botster bug report",
            body = report.title,
            action = {
                kind = "mcp_tool",
                name = "receive_messages",
                params = {},
            },
        })
    end)

    return result
end

mcp.tool("file_botster_bug", {
    description = "File a Botster bug report with the Botster Bugs Codex orchestrator. The plugin keeps no durable records; the orchestrator creates Project Pipelines tickets.",
    input_schema = {
        type = "object",
        properties = {
            title = { type = "string", description = "Short bug title." },
            description = { type = "string", description = "What went wrong and why it matters." },
            severity = { type = "string", enum = { "blocker", "high", "medium", "low", "info", "bug" } },
            reproduction_steps = {
                type = "array",
                items = { type = "string" },
                description = "Concrete steps to reproduce.",
            },
            expected = { type = "string" },
            actual = { type = "string" },
            evidence = { type = "string", description = "Logs, command output, file paths, screenshots, or observations." },
            source = { type = "string", description = "Optional source context, such as the feature, command, UI, or agent that observed the bug." },
            target_id = { type = "string", description = "Optional spawn target ID. Defaults to the caller's target, then a trybotster target if present." },
            target_name = { type = "string", description = "Optional spawn target name when target_id is not known." },
        },
        required = { "title", "description" },
    },
}, function(params, context)
    params = params or {}
    context = context or {}
    if is_blank(params.title) then error("title is required") end
    if is_blank(params.description) then error("description is required") end

    local caller = current_caller(context)
    local target, target_err = resolve_target(params, caller)
    if not target then error(target_err) end

    local report = report_payload(params, context, caller, target)
    local existing = live_orchestrator(target)
    if existing then
        local delivered = post_to_orchestrator(existing.session_uuid, report, context)
        return {
            ok = true,
            routed = "existing_orchestrator",
            session_uuid = existing.session_uuid,
            message_status = delivered and delivered.status or nil,
            message_id = delivered and delivered.msg_id or nil,
        }
    end

    local request_id = "botster-bugs:orchestrator:" .. tostring(os.time())
    local created = Hub.get():create_agent{
        request_id = request_id,
        agent_name = DEFAULT_AGENT_NAME,
        issue_or_branch = "botster-bugs",
        target_id = target.target_id,
        target_path = target.target_path,
        target_repo = target.target_repo,
        workspace_name = WORKSPACE_NAME,
        label = ORCHESTRATOR_LABEL,
        prompt = orchestrator_prompt(report),
        metadata = {
            owner_plugin = OWNER,
            visibility = "workspace",
            surface = OWNER,
            role = "orchestrator",
        },
    }

    return {
        ok = true,
        routed = "new_orchestrator",
        request_id = created and created.request_id or request_id,
        session_uuid = created and (created.session_uuid or created.id) or nil,
        status = created and created.status or "pending",
    }
end)

log.info("[botster-bugs] Plugin loaded")

return {}
