-- MCP (Model Context Protocol) tool registry
--
-- Allows any Lua plugin to register tools that agents can invoke via MCP.
-- Tools are registered with a name, JSON Schema descriptor, and a handler
-- function. The registry notifies connected MCP clients when the tool
-- list changes so they can re-fetch.
--
-- Tools track their source (plugin path) automatically. On plugin reload,
-- call mcp.reset(source) to clear that plugin's tools before re-registering.
--
-- Usage from a plugin:
--   mcp.tool("list_hubs", {
--       description = "List all connected hubs",
--       input_schema = { type = "object", properties = {} },
--   }, function(params)
--       return hub.list()
--   end)

local M = {}

-- Internal tool registry: name -> { name, description, input_schema, handler, source }
local tools = {}

-- Batch mode: when true, mcp_tools_changed is suppressed in tool() and reset().
-- end_batch() clears the flag and emits once. Used by loader.reload_plugin() to
-- collapse N notifications (1 reset + N tool registrations) into a single emit.
local _batch = false

--- Get the current plugin source.
-- Reads _G._loading_plugin_source set by loader.lua during plugin load/reload.
-- @return string Source identifier (file path or "unknown")
local function caller_source()
    return _G._loading_plugin_source or "unknown"
end

--- Register an MCP tool.
-- @param name string Tool name (e.g. "list_hubs")
-- @param schema table { description = "...", input_schema = { type = "object", ... } }
-- @param handler function(params) -> result (table or string)
function M.tool(name, schema, handler)
    if type(name) ~= "string" or name == "" then
        error("mcp.tool: name must be a non-empty string")
    end
    if type(schema) ~= "table" then
        error("mcp.tool: schema must be a table")
    end
    if type(handler) ~= "function" then
        error("mcp.tool: handler must be a function")
    end

    tools[name] = {
        name = name,
        description = schema.description or "",
        input_schema = schema.input_schema or { type = "object", properties = {} },
        handler = handler,
        source = caller_source(),
    }

    if not _batch then
        events.emit("mcp_tools_changed")
    end
    log.info(string.format("MCP tool registered: %s", name))
end

--- Remove an MCP tool by name.
-- @param name string Tool name
function M.remove_tool(name)
    if tools[name] then
        tools[name] = nil
        events.emit("mcp_tools_changed")
        log.info(string.format("MCP tool removed: %s", name))
    end
end

--- Clear tools registered by a specific source.
-- Called automatically before plugin reload. If source is nil, clears all.
-- @param source string|nil Source identifier to clear (nil = clear all)
function M.reset(source)
    local removed = 0
    if source then
        for name, tool in pairs(tools) do
            if tool.source == source then
                tools[name] = nil
                removed = removed + 1
            end
        end
    else
        for name in pairs(tools) do
            tools[name] = nil
            removed = removed + 1
        end
    end
    if removed > 0 then
        if not _batch then
            events.emit("mcp_tools_changed")
        end
        log.info(string.format("MCP tools reset: %d removed (source=%s)", removed, tostring(source)))
    end
end

--- Begin a batch tool update — suppress mcp_tools_changed during reset + registration.
-- Always pair with end_batch(). Use pcall around load_plugin so end_batch() runs
-- even on failure — leaving batch mode stuck would permanently silence notifications.
function M.begin_batch()
    _batch = true
end

--- End a batch tool update — emit mcp_tools_changed exactly once.
-- Correct even on load failure: tools were cleared by reset(), clients need to know.
function M.end_batch()
    _batch = false
    events.emit("mcp_tools_changed")
end

--- List all registered tools (metadata only, no handlers).
-- @return array of { name, description, input_schema }
function M.list_tools()
    local result = {}
    for _, tool in pairs(tools) do
        result[#result + 1] = {
            name = tool.name,
            description = tool.description,
            input_schema = tool.input_schema,
        }
    end
    return result
end

--- Call a tool by name.
-- @param name string Tool name
-- @param params table Arguments from the MCP client
-- @param context table|nil Caller context (caller_cwd, etc.)
-- @return result, error_string
function M.call_tool(name, params, context)
    local tool = tools[name]
    if not tool then
        return nil, "Unknown tool: " .. name
    end

    local ok, result = pcall(tool.handler, params or {}, context or {})
    if not ok then
        return nil, tostring(result)
    end

    -- Normalize result: if it's a string, wrap it
    if type(result) == "string" then
        return { { type = "text", text = result } }
    elseif type(result) == "table" then
        -- If result is already MCP content format (array of {type, text}), pass through
        -- Otherwise, JSON-encode and wrap
        if result[1] and result[1].type then
            return result
        else
            return { { type = "text", text = json.encode(result) } }
        end
    else
        return { { type = "text", text = tostring(result) } }
    end
end

--- Get count of registered tools.
-- @return number
function M.count()
    local n = 0
    for _ in pairs(tools) do n = n + 1 end
    return n
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================
-- mcp.lua holds plugin-registered tool handlers. On reload:
--   _before_reload: save tools table (handlers are plugin closures, not mcp code)
--   _after_reload:  restore tools, update _G.mcp so existing call sites see new module
-- Without these hooks, reloading mcp.lua silently orphans all registered tools.

function M._before_reload()
    -- Stash the tool registry via hub.state (in-memory, handles survive reload)
    if _G.state then
        _G.state.set("mcp_tools_saved", tools)
    end
    log.info(string.format("mcp.lua reloading — saving %d tools", M.count()))
end

function M._after_reload()
    -- Restore tools from before reload
    if _G.state then
        local saved = _G.state.get("mcp_tools_saved", {})
        tools = saved
        _G.state.set("mcp_tools_saved", nil)
    end
    -- Update global so callers using _G.mcp get new module methods
    _G.mcp = M
    log.info(string.format("mcp.lua reloaded — %d tools preserved", M.count()))
end

return M
