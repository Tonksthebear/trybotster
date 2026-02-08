-- Command registry for hub channel commands (hot-reloadable)
--
-- Registration-based dispatch system for hub commands. Built-in commands
-- are registered by handlers/commands.lua; users can register custom
-- commands from user/init.lua or hooks.
--
-- Analogous to Neovim's nvim_create_user_command(): an open registry
-- that replaces the closed if/elseif dispatch chain.
--
-- Module-local registry is re-populated when handlers/commands.lua
-- reloads (the loader reloads dependents after libs).

local M = {}

-- Command registry: cmd_type -> { handler, description }
local registry = {}

--- Register a hub command handler.
-- Overwrites any existing handler for the same command type.
-- @param cmd_type string The command type (e.g., "list_agents")
-- @param handler function Called with (client, sub_id, command)
-- @param opts? table { description = "..." }
function M.register(cmd_type, handler, opts)
    assert(type(cmd_type) == "string", "cmd_type must be a string")
    assert(type(handler) == "function", "handler must be a function")
    opts = opts or {}
    registry[cmd_type] = {
        handler = handler,
        description = opts.description or "",
    }
    log.debug(string.format("Command registered: %s", cmd_type))
end

--- Unregister a hub command handler.
-- @param cmd_type string The command type to remove
-- @return boolean True if a handler was removed
function M.unregister(cmd_type)
    if registry[cmd_type] then
        registry[cmd_type] = nil
        log.debug(string.format("Command unregistered: %s", cmd_type))
        return true
    end
    return false
end

--- Dispatch a command to its registered handler.
-- Fires hooks.notify("after_hub_command") after execution for observability.
-- @param client The Client instance
-- @param sub_id The subscription ID for responses
-- @param command The command table (must have .type or .command)
function M.dispatch(client, sub_id, command)
    local cmd_type = command.type or command.command

    local entry = registry[cmd_type]
    if entry then
        local ok, err = pcall(entry.handler, client, sub_id, command)
        if not ok then
            log.error(string.format("Command '%s' error: %s", cmd_type, tostring(err)))
        end
        hooks.notify("after_hub_command", {
            command = cmd_type,
            client = client,
            sub_id = sub_id,
            success = ok,
            error = not ok and err or nil,
        })
    else
        log.debug(string.format("Unknown hub command: %s", tostring(cmd_type)))
    end
end

--- Check if a command is registered.
-- @param cmd_type string The command type
-- @return boolean
function M.has(cmd_type)
    return registry[cmd_type] ~= nil
end

--- List all registered commands.
-- @return table Array of { command, description }
function M.list()
    local result = {}
    for cmd_type, entry in pairs(registry) do
        table.insert(result, {
            command = cmd_type,
            description = entry.description,
        })
    end
    table.sort(result, function(a, b) return a.command < b.command end)
    return result
end

--- Get the count of registered commands.
-- @return number
function M.count()
    local n = 0
    for _ in pairs(registry) do
        n = n + 1
    end
    return n
end

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info(string.format("commands.lua reloading (%d commands registered)", M.count()))
end

function M._after_reload()
    log.info("commands.lua reloaded")
end

return M
