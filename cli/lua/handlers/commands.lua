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

commands.register("select_agent", function(_client, _sub_id, command)
    -- No backend action needed; agent selection is client-side UI state
    log.debug(string.format("Select agent: %s", tostring(command.id or command.agent_index)))
end, { description = "Select agent (client-side only, no-op)" })

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
