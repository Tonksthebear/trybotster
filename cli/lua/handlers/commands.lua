-- Built-in hub command registrations (hot-reloadable)
--
-- Registers all built-in hub channel commands with the command registry.
-- Extracted from lib/client.lua's handle_hub_data() if/elseif chain.
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

commands.register("list_profiles", function(client, sub_id, _command)
    local ConfigResolver = require("lib.config_resolver")
    local device_root = config.data_dir and config.data_dir() or nil
    local repo_root = worktree.repo_root()
    local profiles = ConfigResolver.list_profiles_all(device_root, repo_root)
    local shared_agent = ConfigResolver.has_agent_without_profile(device_root, repo_root)
    client:send({
        subscriptionId = sub_id,
        type = "profiles",
        profiles = profiles,
        shared_agent = shared_agent,
    })
end, { description = "List available config profiles" })

-- ============================================================================
-- Agent Lifecycle Commands
-- ============================================================================

commands.register("create_agent", function(client, _sub_id, command)
    local issue_or_branch = command.issue_or_branch or command.branch
    local prompt = command.prompt
    local from_worktree = command.from_worktree
    local profile = command.profile

    require("handlers.agents").handle_create_agent(issue_or_branch, prompt, from_worktree, client, profile)
    log.info(string.format("Create agent request: %s (profile: %s)",
        tostring(issue_or_branch or "main"), tostring(profile or "auto")))
end, { description = "Create a new agent (with optional worktree and profile)" })

commands.register("reopen_worktree", function(client, _sub_id, command)
    local path = command.path
    local branch = command.branch or ""
    local prompt = command.prompt

    if path then
        require("handlers.agents").handle_create_agent(branch, prompt, path, client, command.profile)
        log.info(string.format("Reopen worktree request: %s", path))
    else
        log.warn("reopen_worktree missing path")
    end
end, { description = "Reopen an existing worktree as an agent" })

commands.register("delete_agent", function(_client, _sub_id, command)
    -- Field name inconsistency: agent ID may be in "id", "agent_id", or "session_key".
    local agent_id = command.id or command.agent_id or command.session_key
    local delete_worktree = command.delete_worktree or false

    if agent_id then
        require("handlers.agents").handle_delete_agent(agent_id, delete_worktree)
        log.info(string.format("Delete agent request: %s", agent_id))
    else
        log.warn("delete_agent missing agent_id")
    end
end, { description = "Delete an agent (optionally with worktree)" })

commands.register("add_session", function(client, sub_id, command)
    local Agent = require("lib.agent")
    local agent_id = command.agent_id or command.id
    if not agent_id then
        log.warn("add_session missing agent_id")
        return
    end

    local agent = Agent.get(agent_id)
    if not agent then
        log.warn("add_session: agent not found: " .. tostring(agent_id))
        return
    end

    local session_type = command.session_type or "shell"
    local session_config

    if session_type == "shell" then
        -- Raw shell: just bash, no init script
        session_config = { name = "shell" }
    else
        -- Configured session type: resolve from profile
        local types = agent:available_session_types()
        local found = nil
        for _, t in ipairs(types) do
            if t.name == session_type then
                found = t
                break
            end
        end
        if found and not found.raw then
            session_config = {
                name = found.name,
                init_script = found.initialization,
                forward_port = found.port_forward,
            }
        else
            -- Fall back to raw shell with the given name
            session_config = { name = session_type }
        end
    end

    local pty_index = agent:add_session(session_config)
    if pty_index then
        -- Broadcast updated agent list so all clients see the new session.
        -- Use agent_list (not agent_created) to avoid TUI auto-focus reset.
        local connections = require("handlers.connections")
        connections.broadcast_hub_event("agent_list", {
            agents = require("lib.agent").all_info(),
        })
        log.info(string.format("Added session '%s' to agent %s at pty_index %d",
            session_config.name, agent_id, pty_index))
    end
end, { description = "Add a PTY session to a running agent" })

commands.register("remove_session", function(client, sub_id, command)
    local Agent = require("lib.agent")
    local agent_id = command.agent_id or command.id
    if not agent_id then
        log.warn("remove_session missing agent_id")
        return
    end

    local pty_index = command.pty_index
    if pty_index == nil then
        log.warn("remove_session missing pty_index")
        return
    end

    local agent = Agent.get(agent_id)
    if not agent then
        log.warn("remove_session: agent not found: " .. tostring(agent_id))
        return
    end

    local ok = agent:remove_session(pty_index)
    if ok then
        -- Broadcast updated agent list so all clients see the removal
        local connections = require("handlers.connections")
        connections.broadcast_hub_event("agent_list", {
            agents = require("lib.agent").all_info(),
        })
        log.info(string.format("Removed session at pty_index %d from agent %s", pty_index, agent_id))
    end
end, { description = "Remove a PTY session from a running agent" })

commands.register("list_session_types", function(client, sub_id, command)
    local Agent = require("lib.agent")
    local agent_id = command.agent_id or command.id
    if not agent_id then
        log.warn("list_session_types missing agent_id")
        return
    end

    local agent = Agent.get(agent_id)
    if not agent then
        log.warn("list_session_types: agent not found: " .. tostring(agent_id))
        return
    end

    local types = agent:available_session_types()
    client:send({
        subscriptionId = sub_id,
        type = "session_types",
        agent_id = agent_id,
        session_types = types,
    })
end, { description = "List available session types for an agent" })

commands.register("select_agent", function(_client, _sub_id, command)
    -- No backend action needed; agent selection is client-side UI state
    log.debug(string.format("Select agent: %s", tostring(command.id or command.agent_index)))
end, { description = "Select agent (client-side only, no-op)" })

commands.register("clear_notification", function(_client, _sub_id, command)
    local agent_index = command.agent_index
    if agent_index == nil then
        log.warn("clear_notification missing agent_index")
        return
    end
    -- Shared clear logic (no pty_input hook — this is agent switching, not typing)
    _clear_agent_notification(agent_index)
end, { description = "Clear notification flag on an agent" })

-- ============================================================================
-- Connection Commands
-- ============================================================================

commands.register("get_connection_code", function(_client, _sub_id, _command)
    -- generate_connection_url() is idempotent (returns cached bundle
    -- unless consumed by a browser, in which case it auto-regenerates).
    connection.generate()
end, { description = "Get or generate connection code with QR" })

commands.register("regenerate_connection_code", function(_client, _sub_id, _command)
    -- Force-regenerate: creates a fresh PreKeyBundle unconditionally
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
    -- On success, process exec-restarts — connection drops, browser reconnects
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
