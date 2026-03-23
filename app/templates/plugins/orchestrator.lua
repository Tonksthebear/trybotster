-- @template Orchestrator
-- @description Connect to other hubs and manage agents remotely
-- @category plugins
-- @dest plugins/orchestrator/init.lua
-- @scope device
-- @version 2.3.0

-- Orchestrator plugin
--
-- Workspace-aware orchestration across hubs: list/create/delete agents,
-- move sessions between workspaces, and rename/list workspaces.
-- Hub connections are transparent — Hub.get(hub_id) auto-connects on demand
-- via hub_discovery. No manual connection management is needed here.
--
-- Runs a periodic cleanup timer to remove connections to hubs that have
-- stopped running (Hub.cleanup_dead()).
--
-- Tools:
--   whoami         — returns the calling agent's identity and hub info
--   list_hubs      — list all running hubs and their agents
--   list_spawn_targets — list admitted spawn targets with capabilities
--   create_agent   — create an agent on any hub (supports target_name + label)
--   list_workspaces — list workspace manifests on a hub
--   rename_workspace — rename a workspace by ID
--   move_agent_workspace — move a live session between workspaces
--   update_session — update a session's label or task
--   delete_agent   — delete an agent on any hub
--   get_pty_snapshot — get terminal content from an agent session
--
-- Handles incoming hub-to-hub RPCs via hub_rpc_request hook.

local state = require("hub.state")
local hooks = require("hub.hooks")
local Agent = require("lib.agent")
local Hub = require("lib.hub")

local self_id = hub.hub_id()

-- Timer handle — stored so _before_reload can cancel it.
local _timer_state = state.get("orchestrator.timer_state", { id = nil })

-- ============================================================================
-- Shared Helpers
-- ============================================================================

--- Resolve agent_label to agent_id. Returns agent_id or nil + error string.
-- For local hubs, iterates Agent.list() directly.
-- For remote hubs, queries the remote agent list via RPC.
local function resolve_agent_id(params)
    if params.agent_id then
        return params.agent_id
    end
    if not params.agent_label then
        return nil, "Either agent_id or agent_label is required"
    end

    if Hub.is_local(params.hub_id) then
        for _, agent in ipairs(Agent.list()) do
            if agent.label == params.agent_label then
                return agent.session_uuid
            end
        end
    else
        local ok, agents = pcall(function()
            return Hub.get(params.hub_id):agent_list()
        end)
        if ok and agents then
            for _, agent in ipairs(agents) do
                if agent.label == params.agent_label then
                    return agent.id
                end
            end
        end
    end

    return nil, string.format("No agent found with label '%s'", params.agent_label)
end

-- ============================================================================
-- MCP Tools
-- ============================================================================

mcp.tool("whoami", {
    description = "Returns the calling agent's identity: agent key, session UUID, hub ID, repo, branch, and worktree path. Use this to discover your own identity when coordinating with other agents or tools.",
    input_schema = {
        type = "object",
        properties = {},
    },
}, function(params, context)
    local caller_id = context.session_uuid or context.agent_key
    local result = {
        session_uuid = caller_id,
        hub_id = context.hub_id,
        self_hub_id = self_id,
    }

    if caller_id and caller_id ~= "" then
        local agent = Agent.get(caller_id)
        if agent then
            local info = agent:info()
            result.session_uuid = agent.session_uuid
            result.repo = info.repo
            result.branch_name = info.branch_name
            result.worktree_path = info.worktree_path
            result.agent_name = info.agent_name
            result.workspace_name = info.workspace_name
            result.status = info.status
            result.label = info.label
            result.task = info.task
        end
    end

    return result
end)

mcp.tool("list_hubs", {
    description = "List all running hubs and their agents. Returns an array of hubs, each with an id, status ('local' or 'connected'), and agents array. Your own agent is excluded from the results. Use this to discover other agents before sending messages or reading snapshots.",
    input_schema = {
        type = "object",
        properties = {},
    },
}, function(params, context)
    local caller_key = context.session_uuid or context.agent_key
    local caller_hub = context.hub_id
    local result = {}

    local function is_caller(agent_id, hub_id)
        return caller_key and caller_key ~= ""
            and caller_hub and caller_hub ~= ""
            and agent_id == caller_key
            and hub_id == caller_hub
    end

    local all_hubs = hub_discovery.list()

    for _, info in ipairs(all_hubs) do
        local h = Hub.get(info.id)
        local ok, agents_raw = pcall(function() return h:agent_list() end)
        local agents = {}
        if ok and agents_raw then
            for _, agent in ipairs(agents_raw) do
                if not is_caller(agent.id, info.id) then
                    table.insert(agents, agent)
                end
            end
        else
            log.warn(string.format("Orchestrator: list_hubs failed to get agents from hub %s: %s",
                info.id, ok and "nil result" or tostring(agents_raw)))
        end

        table.insert(result, {
            id     = info.id,
            status = (info.id == self_id) and "local" or "connected",
            agents = agents,
        })
    end

    return result
end)

mcp.tool("list_spawn_targets", {
    description = "List all admitted spawn targets on the local hub. Returns path, name, enabled status, and git capabilities for each target. Use target IDs from this list when calling create_agent.",
    input_schema = {
        type = "object",
        properties = {},
    },
}, function(params)
    local targets = spawn_targets.list()
    local result = {}
    for _, t in ipairs(targets) do
        local entry = {
            id = t.id,
            name = t.name,
            path = t.path,
            enabled = t.enabled,
        }
        -- Add live git capabilities
        local inspection = spawn_targets.inspect(t.path)
        if inspection then
            entry.is_git_repo = inspection.is_git_repo
            entry.repo_name = inspection.repo_name
            entry.current_branch = inspection.current_branch
            entry.has_botster_dir = inspection.has_botster_dir
        end
        table.insert(result, entry)
    end
    return result
end)

mcp.tool("create_agent", {
    description = "Create a new agent on a hub. For the local hub, omit hub_id or pass the local hub's ID. For remote hubs, pass their hub_id (from list_hubs). Returns the created agent's info. Use list_spawn_targets to get valid target_id values.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID to create the agent on. Omit or pass local hub ID for local creation.",
            },
            issue_or_branch = {
                type = "string",
                description = "Issue number or branch name for the agent.",
            },
            prompt = {
                type = "string",
                description = "Task prompt for the agent.",
            },
            profile = {
                type = "string",
                description = "Config profile name. Omit to auto-select.",
            },
            workspace_id = {
                type = "string",
                description = "Target workspace ID for the new agent.",
            },
            workspace_name = {
                type = "string",
                description = "Target workspace name for the new agent (used if workspace_id is omitted).",
            },
            target_id = {
                type = "string",
                description = "Spawn target ID. Use list_spawn_targets to get valid values.",
            },
            target_name = {
                type = "string",
                description = "Spawn target name (e.g. 'trybotster'). Alternative to target_id — resolved by name lookup.",
            },
            label = {
                type = "string",
                description = "Human-readable label for the agent (e.g. 'rust-plugins-field'). Set on the session after creation.",
            },
        },
        required = { "issue_or_branch" },
    },
}, function(params)
    local target = nil

    -- Resolve target: prefer target_id, fall back to target_name lookup
    local resolved_id = params.target_id
    if not resolved_id and params.target_name then
        local all = spawn_targets.list()
        for _, t in ipairs(all) do
            if t.name == params.target_name then
                resolved_id = t.id
                break
            end
        end
        if not resolved_id then
            return { error = string.format("No spawn target found with name '%s'", params.target_name) }
        end
    end

    if resolved_id then
        local info = spawn_targets.get(resolved_id)
        if info then
            target = {
                target_id = info.id,
                target_path = info.path,
            }
            -- Derive repo name if git-backed
            local inspection = spawn_targets.inspect(info.path)
            if inspection and inspection.repo_name then
                target.target_repo = inspection.repo_name
            end
        end
    end

    return Hub.call_safely(params.hub_id, function()
        local h = Hub.get(params.hub_id)
        local result = h:create_agent(
            params.issue_or_branch,
            params.prompt,
            params.profile,
            params.workspace_id,
            params.workspace_name,
            target
        )

        -- Set label on the newly created session if provided
        if params.label and result and result.id then
            pcall(function()
                h:update_session(result.id, { label = params.label })
            end)
            result.label = params.label
        end

        return result
    end)
end)

mcp.tool("list_workspaces", {
    description = "List workspaces on a hub. Returns persisted workspace metadata with current running-session membership counts.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID to query. Omit for local hub.",
            },
        },
    },
}, function(params)
    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):list_workspaces()
    end)
end)

mcp.tool("rename_workspace", {
    description = "Rename a workspace by ID on a hub.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the workspace exists. Omit for local hub.",
            },
            workspace_id = {
                type = "string",
                description = "Workspace ID to rename.",
            },
            new_name = {
                type = "string",
                description = "New workspace display name.",
            },
        },
        required = { "workspace_id", "new_name" },
    },
}, function(params)
    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):rename_workspace(params.workspace_id, params.new_name)
    end)
end)

mcp.tool("move_agent_workspace", {
    description = "Move a live session to another workspace by ID or name.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the session lives. Omit for local hub.",
            },
            agent_id = {
                type = "string",
                description = "Session UUID or agent key to move.",
            },
            agent_label = {
                type = "string",
                description = "Agent label to resolve. Alternative to agent_id.",
            },
            workspace_id = {
                type = "string",
                description = "Target workspace ID.",
            },
            workspace_name = {
                type = "string",
                description = "Target workspace name (used when workspace_id is omitted).",
            },
        },
    },
}, function(params)
    local target_agent_id, err = resolve_agent_id(params)
    if not target_agent_id then return { error = err } end

    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):move_agent_workspace(
            target_agent_id,
            params.workspace_id,
            params.workspace_name
        )
    end)
end)

mcp.tool("update_session", {
    description = table.concat({
        "Update metadata on a running session. Use this to keep the hub informed about what you are working on.",
        "",
        "Updatable fields:",
        "  label — A short human-readable tag describing the session's purpose (e.g. 'auth bug fix', 'PR #42 review', 'deploy monitoring').",
        "         Shown alongside the session name in the agent list. Set by the user or by an orchestrator assigning work.",
        "         Pass an empty string to clear.",
        "  task  — Your current activity, updated as your work progresses (e.g. 'running test suite', 'waiting for CI', 'rebasing after conflict').",
        "         This is YOUR self-report — call update_session with a new task whenever your focus shifts.",
        "         Pass an empty string to clear.",
        "",
        "Both fields are optional per call — pass only what changed. Updates are broadcast to all connected clients immediately.",
        "To update your own session, call whoami first to get your agent_id.",
    }, "\n"),
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the session lives. Omit for local hub.",
            },
            agent_id = {
                type = "string",
                description = "Session UUID or agent key to update.",
            },
            agent_label = {
                type = "string",
                description = "Agent label to resolve. Alternative to agent_id.",
            },
            label = {
                type = "string",
                description = "Short human-readable tag for the session's purpose (e.g. 'auth bug fix'). Pass empty string to clear.",
            },
            task = {
                type = "string",
                description = "Current activity self-report (e.g. 'running test suite'). Update whenever your focus shifts. Pass empty string to clear.",
            },
        },
    },
}, function(params)
    local target_agent_id, err = resolve_agent_id(params)
    if not target_agent_id then return { error = err } end

    local fields = {}
    if params.label ~= nil then fields.label = params.label end
    if params.task ~= nil then fields.task = params.task end

    if not next(fields) then
        return { error = "No fields to update. Pass label and/or task." }
    end

    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):update_session(target_agent_id, fields)
    end)
end)

mcp.tool("delete_agent", {
    description = "Delete an agent on a hub. Pass agent_id (agent key) or agent_label from list_hubs results. Optionally delete the git worktree too.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the agent lives. Omit for local.",
            },
            agent_id = {
                type = "string",
                description = "Agent key/ID to delete.",
            },
            agent_label = {
                type = "string",
                description = "Agent label to resolve. Alternative to agent_id.",
            },
            delete_worktree = {
                type = "boolean",
                description = "Also delete the git worktree. Default false.",
            },
        },
    },
}, function(params)
    local target_agent_id, err = resolve_agent_id(params)
    if not target_agent_id then return { error = err } end

    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):delete_agent(
            target_agent_id,
            params.delete_worktree
        )
    end)
end)

mcp.tool("get_pty_snapshot", {
    description = "Get a PTY snapshot from an agent session on any hub. Returns the current terminal content as text. Use agent_id (agent key) or agent_label from list_hubs results. Omit hub_id for the local hub.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the agent lives. Omit for local hub.",
            },
            agent_id = {
                type = "string",
                description = "Agent key/ID.",
            },
            agent_label = {
                type = "string",
                description = "Agent label to resolve. Alternative to agent_id.",
            },
            session = {
                type = "string",
                description = "Session name (default: 'agent').",
            },
        },
    },
}, function(params)
    local target_agent_id, err = resolve_agent_id(params)
    if not target_agent_id then return { error = err } end

    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):get_pty_snapshot(target_agent_id, params.session)
    end)
end)

-- ============================================================================
-- Hub-to-Hub RPC (incoming requests from remote hubs)
-- ============================================================================

-- Handle RPC requests from remote hubs via the socket server.
-- Dispatches by message.type and sends the response back with _mcp_rid.
hooks.on("hub_rpc_request", "orchestrator_rpc", function(client_id, message)
    local rid = message._mcp_rid
    local local_hub = Hub.get()

    local function respond(fn)
        local ok, result = pcall(fn)
        if ok then
            socket.send(client_id, { _mcp_rid = rid, result = result })
        else
            socket.send(client_id, { _mcp_rid = rid, error = tostring(result) })
        end
    end

    -- Resolve agent_label to agent_id for incoming RPCs
    local function resolve_rpc_agent_id(msg)
        if msg.agent_id then return msg.agent_id end
        if not msg.agent_label then return nil end
        for _, agent in ipairs(Agent.list()) do
            if agent.label == msg.agent_label then
                return agent.session_uuid
            end
        end
        return nil
    end

    if message.type == "send_message" then
        respond(function()
            return local_hub:send_message(message.agent_id, message.text, message.session)
        end)
    elseif message.type == "get_pty_snapshot" then
        respond(function()
            local aid = resolve_rpc_agent_id(message)
            if not aid then error(string.format("No agent found with label '%s'", message.agent_label or "nil")) end
            return local_hub:get_pty_snapshot(aid, message.session)
        end)
    elseif message.type == "create_agent" then
        respond(function()
            local target = nil
            if message.target_id then
                target = {
                    target_id = message.target_id,
                    target_path = message.target_path,
                    target_repo = message.target_repo,
                }
            end
            return local_hub:create_agent(
                message.issue_or_branch,
                message.prompt,
                message.profile,
                message.workspace_id,
                message.workspace_name,
                target
            )
        end)
    elseif message.type == "list_workspaces" then
        respond(function()
            return local_hub:list_workspaces()
        end)
    elseif message.type == "rename_workspace" then
        respond(function()
            return local_hub:rename_workspace(message.workspace_id, message.new_name)
        end)
    elseif message.type == "move_agent_workspace" then
        respond(function()
            local aid = resolve_rpc_agent_id(message)
            if not aid then error(string.format("No agent found with label '%s'", message.agent_label or "nil")) end
            return local_hub:move_agent_workspace(
                aid,
                message.workspace_id,
                message.workspace_name
            )
        end)
    elseif message.type == "update_session" then
        respond(function()
            local aid = resolve_rpc_agent_id(message)
            if not aid then error(string.format("No agent found with label '%s'", message.agent_label or "nil")) end
            local fields = {}
            if message.label ~= nil then fields.label = message.label end
            if message.task ~= nil then fields.task = message.task end
            return local_hub:update_session(aid, fields)
        end)
    elseif message.type == "delete_agent" then
        respond(function()
            local aid = resolve_rpc_agent_id(message)
            if not aid then error(string.format("No agent found with label '%s'", message.agent_label or "nil")) end
            return local_hub:delete_agent(aid, message.delete_worktree)
        end)
    elseif message.type == "post_message" then
        respond(function()
            return local_hub:post(message.agent_id, {
                type          = message.msg_type,
                payload       = message.payload,
                reply_to      = message.reply_to,
                expires_in    = message.expires_in,
                session       = message.session,
                from_agent_id = message.from_agent_id,
            })
        end)
    elseif message.type == "receive_messages" then
        respond(function()
            return local_hub:receive_messages(message.agent_id)
        end)
    elseif message.type == "get_agent_list" then
        respond(function()
            return Agent.all_info()
        end)
    else
        socket.send(client_id, {
            _mcp_rid = rid,
            error = string.format("unknown RPC method: %s", tostring(message.type)),
        })
    end
end)

-- ============================================================================
-- Lifecycle
-- ============================================================================

_timer_state.id = timer.every(60, function()
    Hub.cleanup_dead()
end)

log.info(string.format("Orchestrator plugin loaded (self=%s)", tostring(self_id)))

return {
    _before_reload = function()
        if _timer_state.id then
            timer.cancel(_timer_state.id)
            _timer_state.id = nil
        end
        hooks.off("hub_rpc_request", "orchestrator_rpc")
        log.info("Orchestrator: reloading")
    end,

    _after_reload = function()
        _timer_state.id = timer.every(60, function()
            Hub.cleanup_dead()
        end)
        log.info("Orchestrator: reloaded")
    end,
}
