-- @template Orchestrator
-- @description Connect to other hubs and manage agents remotely
-- @category plugins
-- @dest plugins/orchestrator/init.lua
-- @scope device
-- @version 2.1.0

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
--   create_agent   — create an agent on any hub
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
-- MCP Tools
-- ============================================================================

mcp.tool("whoami", {
    description = "Returns the calling agent's identity: agent key, session UUID, hub ID, repo, branch, and worktree path. Use this to discover your own identity when coordinating with other agents or tools.",
    input_schema = {
        type = "object",
        properties = {},
    },
}, function(params, context)
    local result = {
        agent_key = context.agent_key,
        hub_id = context.hub_id,
        self_hub_id = self_id,
    }

    if context.agent_key and context.agent_key ~= "" then
        local agent = Agent.find_by_agent_key(context.agent_key)
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
    local caller_key = context.agent_key
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

mcp.tool("create_agent", {
    description = "Create a new agent on a hub. For the local hub, omit hub_id or pass the local hub's ID. For remote hubs, pass their hub_id (from list_hubs). Returns the created agent's info.",
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
        },
        required = { "issue_or_branch" },
    },
}, function(params)
    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):create_agent(
            params.issue_or_branch,
            params.prompt,
            params.profile,
            params.workspace_id,
            params.workspace_name
        )
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
            workspace_id = {
                type = "string",
                description = "Target workspace ID.",
            },
            workspace_name = {
                type = "string",
                description = "Target workspace name (used when workspace_id is omitted).",
            },
        },
        required = { "agent_id" },
    },
}, function(params)
    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):move_agent_workspace(
            params.agent_id,
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
            label = {
                type = "string",
                description = "Short human-readable tag for the session's purpose (e.g. 'auth bug fix'). Pass empty string to clear.",
            },
            task = {
                type = "string",
                description = "Current activity self-report (e.g. 'running test suite'). Update whenever your focus shifts. Pass empty string to clear.",
            },
        },
        required = { "agent_id" },
    },
}, function(params)
    local fields = {}
    if params.label ~= nil then fields.label = params.label end
    if params.task ~= nil then fields.task = params.task end

    if not next(fields) then
        return { error = "No fields to update. Pass label and/or task." }
    end

    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):update_session(params.agent_id, fields)
    end)
end)

mcp.tool("delete_agent", {
    description = "Delete an agent on a hub. Pass the agent_id (agent key) from list_hubs results. Optionally delete the git worktree too.",
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
            delete_worktree = {
                type = "boolean",
                description = "Also delete the git worktree. Default false.",
            },
        },
        required = { "agent_id" },
    },
}, function(params)
    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):delete_agent(
            params.agent_id,
            params.delete_worktree
        )
    end)
end)

mcp.tool("get_pty_snapshot", {
    description = "Get a PTY snapshot from an agent session on any hub. Returns the current terminal content as text. Use the agent_id (agent key) from list_hubs results. Omit hub_id for the local hub.",
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
            session = {
                type = "string",
                description = "Session name (default: 'agent').",
            },
        },
        required = { "agent_id" },
    },
}, function(params)
    return Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):get_pty_snapshot(params.agent_id, params.session)
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

    if message.type == "send_message" then
        respond(function()
            return local_hub:send_message(message.agent_id, message.text, message.session)
        end)
    elseif message.type == "get_pty_snapshot" then
        respond(function()
            return local_hub:get_pty_snapshot(message.agent_id, message.session)
        end)
    elseif message.type == "create_agent" then
        respond(function()
            return local_hub:create_agent(
                message.issue_or_branch,
                message.prompt,
                message.profile,
                message.workspace_id,
                message.workspace_name
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
            return local_hub:move_agent_workspace(
                message.agent_id,
                message.workspace_id,
                message.workspace_name
            )
        end)
    elseif message.type == "update_session" then
        respond(function()
            local fields = {}
            if message.label ~= nil then fields.label = message.label end
            if message.task ~= nil then fields.task = message.task end
            return local_hub:update_session(message.agent_id, fields)
        end)
    elseif message.type == "delete_agent" then
        respond(function()
            return local_hub:delete_agent(message.agent_id, message.delete_worktree)
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
