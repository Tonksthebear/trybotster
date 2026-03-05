-- Built-in hub command registrations (hot-reloadable)
--
-- Registers all built-in hub channel commands with the command registry.
--
-- Users can override built-in commands or add new ones:
--   local commands = require("lib.commands")
--   commands.register("my_command", function(client, sub_id, command)
--       client:send({ subscriptionId = sub_id, type = "my_response", data = "hello" })
--   end)

local commands = require("lib.commands")

-- ============================================================================
-- Query Commands
-- ============================================================================

commands.register("list_agents", function(client, sub_id, _command)
    client:send_agent_list(sub_id)
end, { description = "Send agent list to client" })

commands.register("list_worktrees", function(client, sub_id, _command)
    client:send_worktree_list(sub_id)
end, { description = "Send worktree list to client" })

local function send_agent_config(client, sub_id)
    local ConfigResolver = require("lib.config_resolver")
    local device_root = config.data_dir and config.data_dir() or nil
    local repo_root = worktree.repo_root()
    local agents = ConfigResolver.list_agents(device_root, repo_root)
    local accessories = ConfigResolver.list_accessories(device_root, repo_root)
    local workspaces = ConfigResolver.list_workspaces(device_root, repo_root)
    client:send({
        subscriptionId = sub_id,
        type = "agent_config",
        agents = agents,
        accessories = accessories,
        workspaces = workspaces,
    })
end

commands.register("list_configs", function(client, sub_id, _command)
    send_agent_config(client, sub_id)
end, { description = "List available agents, accessories, and workspaces" })

commands.register("list_agent_config", function(client, sub_id, _command)
    send_agent_config(client, sub_id)
end, { description = "List available agent config (alias for list_configs)" })

-- Backward compat: list_profiles → list_configs
commands.register("list_profiles", function(client, sub_id, _command)
    local ConfigResolver = require("lib.config_resolver")
    local device_root = config.data_dir and config.data_dir() or nil
    local repo_root = worktree.repo_root()
    local agents = ConfigResolver.list_agents(device_root, repo_root)
    client:send({
        subscriptionId = sub_id,
        type = "profiles",
        profiles = agents,
        shared_agent = #agents > 0,
    })
end, { description = "List available config profiles (deprecated, use list_configs)" })

-- ============================================================================
-- Agent Lifecycle Commands
-- ============================================================================

commands.register("create_agent", function(client, _sub_id, command)
    local issue_or_branch = command.issue_or_branch or command.branch
    local prompt = command.prompt
    local from_worktree = command.from_worktree
    -- Accept both "agent_name" (new) and "profile" (legacy)
    local agent_name = command.agent_name or command.profile
    local workspace = command.workspace

    local metadata = nil
    if workspace then
        metadata = { workspace = workspace }
    end

    -- If a workspace config name is provided, load the manifest
    -- Browser sends "workspace", CLI may also use "workspace_config"
    local workspace_config_name = command.workspace_config or workspace
    if workspace_config_name then
        metadata = metadata or {}
        local ConfigResolver = require("lib.config_resolver")
        local device_root = config.data_dir and config.data_dir() or nil
        local repo_root = worktree.repo_root()
        local resolved = ConfigResolver.resolve_all({
            device_root = device_root,
            repo_root = repo_root,
            require_agent = false,
        })
        if resolved and resolved.workspaces[workspace_config_name] then
            metadata.workspace_config = resolved.workspaces[workspace_config_name]
        end
    end

    require("handlers.agents").handle_create_agent(issue_or_branch, prompt, from_worktree, client, agent_name, metadata)
    log.info(string.format("Create agent request: %s (agent: %s, workspace: %s)",
        tostring(issue_or_branch or "main"), tostring(agent_name or "auto"), tostring(workspace or "none")))
end, { description = "Create a new agent (with optional worktree, agent name, and workspace)" })

commands.register("create_accessory", function(client, _sub_id, command)
    -- Accept both "accessory_name" (new) and "session_name" (legacy)
    local accessory_name = command.accessory_name or command.session_name or command.name
    local workspace = command.workspace
    local agent_name = command.agent_name or command.profile
    local metadata = command.metadata

    if not accessory_name then
        log.warn("create_accessory missing accessory_name")
        return
    end

    require("handlers.agents").handle_create_accessory(workspace, accessory_name, agent_name, metadata)
    log.info(string.format("Create accessory request: %s (workspace: %s)",
        accessory_name, tostring(workspace or "none")))
end, { description = "Create an accessory session (no AI autonomy)" })

commands.register("rename_workspace", function(client, sub_id, command)
    local workspace_id = command.workspace_id
    local new_name = command.new_name or command.name
    if not workspace_id or not new_name then
        log.warn("rename_workspace missing workspace_id or new_name")
        return
    end

    local data_dir = config.data_dir and config.data_dir() or nil
    if not data_dir then
        log.warn("rename_workspace: no data_dir configured")
        return
    end

    local ws = require("lib.workspace_store")
    local ok = ws.rename_workspace(data_dir, workspace_id, new_name)
    if ok then
        local connections = require("handlers.connections")
        connections.broadcast_hub_event("agent_list", {
            agents = require("lib.agent").all_info(),
        })
        log.info(string.format("Workspace %s renamed to '%s'", workspace_id, new_name))
    end
end, { description = "Rename a workspace" })

commands.register("reopen_worktree", function(client, _sub_id, command)
    local path = command.path
    local branch = command.branch or ""
    local prompt = command.prompt

    if path then
        local agent_name = command.agent_name or command.profile
        require("handlers.agents").handle_create_agent(branch, prompt, path, client, agent_name)
        log.info(string.format("Reopen worktree request: %s", path))
    else
        log.warn("reopen_worktree missing path")
    end
end, { description = "Reopen an existing worktree as an agent" })

commands.register("delete_agent", function(_client, _sub_id, command)
    local session_id = command.id or command.agent_id or command.session_uuid or command.session_key
    local delete_worktree = command.delete_worktree or false

    if session_id then
        require("handlers.agents").handle_delete_session(session_id, delete_worktree)
        log.info(string.format("Delete session request: %s", session_id))
    else
        log.warn("delete_agent missing session identifier")
    end
end, { description = "Delete a session (agent or accessory, optionally with worktree)" })

-- Alias: delete_session → delete_agent
commands.register("delete_session", function(_client, _sub_id, command)
    local session_id = command.id or command.session_uuid or command.agent_id or command.session_key
    local delete_worktree = command.delete_worktree or false

    if session_id then
        require("handlers.agents").handle_delete_session(session_id, delete_worktree)
        log.info(string.format("Delete session request: %s", session_id))
    else
        log.warn("delete_session missing session identifier")
    end
end, { description = "Delete a session (alias for delete_agent)" })

commands.register("select_agent", function(_client, _sub_id, command)
    log.debug(string.format("Select agent: %s", tostring(command.id or command.session_uuid)))
end, { description = "Select agent (client-side only, no-op)" })

commands.register("clear_notification", function(_client, _sub_id, command)
    local session_uuid = command.session_uuid
    if session_uuid then
        _clear_session_notification(session_uuid)
    else
        log.warn("clear_notification missing session_uuid")
    end
end, { description = "Clear notification flag on a session" })

-- ============================================================================
-- Connection Commands
-- ============================================================================

commands.register("get_connection_code", function(_client, _sub_id, _command)
    connection.generate()
end, { description = "Get or generate connection code with QR" })

commands.register("regenerate_connection_code", function(_client, _sub_id, _command)
    connection.regenerate()
    log.info("Connection code regeneration requested")
end, { description = "Force-regenerate connection code" })

commands.register("copy_connection_url", function(_client, _sub_id, _command)
    connection.copy_to_clipboard()
end, { description = "Copy connection URL to clipboard" })

-- ============================================================================
-- Hub Control Commands
-- ============================================================================

commands.register("quit", function(_client, _sub_id, _command)
    hub.quit()
end, { description = "Shut down the hub" })

commands.register("restart_hub", function(_client, _sub_id, _command)
    hub.exec_restart()
end, { description = "Graceful restart — agents survive the Hub restarting" })

commands.register("dev_rebuild", function(_client, _sub_id, _command)
    hub.dev_rebuild()
end, { description = "Dev: cargo build then exec-restart — agents survive (requires cargo on PATH)" })

-- ============================================================================
-- Update Commands
-- ============================================================================

commands.register("check_update", function(client, sub_id, _command)
    local ok, status = pcall(update.check)
    if not ok then
        client:send({
            subscriptionId = sub_id,
            type = "update_error",
            error = tostring(status),
        })
        return
    end
    local agents = require("lib.agent").all_info()
    local active_count = 0
    for _, agent in ipairs(agents) do
        if agent.status ~= "closed" then active_count = active_count + 1 end
    end
    client:send({
        subscriptionId = sub_id,
        type = "update_status",
        status = status.status,
        current = status.current,
        latest = status.latest,
        active_agents = active_count,
    })
end, { description = "Check for CLI updates" })

commands.register("install_update", function(client, sub_id, _command)
    local result = update.install()
    if result.error then
        client:send({
            subscriptionId = sub_id,
            type = "update_error",
            error = result.error,
        })
    end
end, { description = "Install update and restart (kills active agents)" })

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("handlers/commands.lua reloading")
end

function M._after_reload()
    log.info(string.format("handlers/commands.lua reloaded (%d commands)", commands.count()))
end

log.info(string.format("Built-in commands registered: %d", commands.count()))

return M
