-- MCP (Model Context Protocol) tool and prompt registry
--
-- Allows any Lua plugin to register tools and prompts that agents can invoke
-- via MCP. Both are registered with a name, descriptor, and handler function.
-- The registry notifies connected MCP clients when the tool or prompt list
-- changes so they can re-fetch.
--
-- Tools and prompts track their source (plugin path) automatically. On plugin
-- reload, call mcp.reset(source) to clear that plugin's registrations before
-- re-registering.
--
-- Usage from a plugin:
--   mcp.tool("list_hubs", {
--       description = "List all connected hubs",
--       input_schema = { type = "object", properties = {} },
--   }, function(params)
--       return hub.list()
--   end)
--
--   mcp.prompt("botster-context", {
--       description = "Inject current hub state as context",
--       arguments = {},
--   }, function(args)
--       return {
--           description = "Current hub state",
--           messages = {
--               { role = "user", content = { type = "text", text = "..." } },
--           },
--       }
--   end)

local M = {}

-- Internal tool registry: name -> { name, description, input_schema, handler, source }
local tools = {}

-- Internal prompt registry: name -> { name, description, arguments, handler, source }
local prompts = {}

-- Batch mode: when true, notifications are suppressed in tool(), prompt(),
-- and reset(). end_batch() clears the flag and schedules only the notifications
-- for registries that actually changed during the batch.
local _batch = false

-- Dirty flags set during a batch when tools or prompts are modified.
-- Cleared by end_batch() after scheduling the relevant notification.
local _batch_tools_dirty = false
local _batch_prompts_dirty = false

-- Debounce state for mcp_tools_changed notifications.
-- Multiple rapid reloads (e.g. several agents editing plugin files simultaneously)
-- each call end_batch(), which would fire N notifications and cause N Claude
-- reconnect cycles. Instead, each call cancels the previous pending timer and
-- schedules a new one — only the final settle fires the notification.
local _debounce_timer = nil

-- Debounce state for mcp_prompts_changed notifications. Mirrors the tool
-- debounce exactly; kept separate because MCP uses distinct list_changed
-- notifications for tools and prompts.
local _debounce_timer_prompts = nil

-- Seconds to wait after the last change before notifying clients.
-- 500 ms covers a burst of simultaneous plugin reloads while keeping
-- updates feeling near-instant for a single file save.
local NOTIFY_DEBOUNCE_SECS = 0.5

-- Schedule (or reschedule) the mcp_tools_changed notification.
-- Cancels any previously pending debounce timer so rapid calls coalesce.
local function schedule_notify()
    if _debounce_timer then
        timer.cancel(_debounce_timer)
    end
    _debounce_timer = timer.after(NOTIFY_DEBOUNCE_SECS, function()
        _debounce_timer = nil
        events.emit("mcp_tools_changed")
    end)
end

-- Schedule (or reschedule) the mcp_prompts_changed notification.
-- Identical debounce pattern to schedule_notify() for tools.
local function schedule_notify_prompts()
    if _debounce_timer_prompts then
        timer.cancel(_debounce_timer_prompts)
    end
    _debounce_timer_prompts = timer.after(NOTIFY_DEBOUNCE_SECS, function()
        _debounce_timer_prompts = nil
        events.emit("mcp_prompts_changed")
    end)
end

--- Get the current plugin source.
-- Reads _G._loading_plugin_source set by loader.lua during plugin load/reload.
-- @return string Source identifier (file path or "unknown")
local function caller_source()
    return _G._loading_plugin_source or "unknown"
end

-- =============================================================================
-- Tools
-- =============================================================================

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
        schedule_notify()
    else
        _batch_tools_dirty = true
    end
    log.info(string.format("MCP tool registered: %s", name))
end

--- Remove an MCP tool by name.
-- @param name string Tool name
function M.remove_tool(name)
    if tools[name] then
        tools[name] = nil
        if not _batch then
            schedule_notify()
        else
            _batch_tools_dirty = true
        end
        log.info(string.format("MCP tool removed: %s", name))
    end
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
-- Prompts
-- =============================================================================

--- Register an MCP prompt.
-- @param name string Prompt name (kebab-case, e.g. "botster-context")
-- @param schema table { description = "...", arguments = { { name, description, required } } }
-- @param handler function(args) -> { description, messages } or string
function M.prompt(name, schema, handler)
    if type(name) ~= "string" or name == "" then
        error("mcp.prompt: name must be a non-empty string")
    end
    if type(schema) ~= "table" then
        error("mcp.prompt: schema must be a table")
    end
    if type(handler) ~= "function" then
        error("mcp.prompt: handler must be a function")
    end

    prompts[name] = {
        name = name,
        description = schema.description or "",
        arguments = schema.arguments or {},
        handler = handler,
        source = caller_source(),
    }

    if not _batch then
        schedule_notify_prompts()
    else
        _batch_prompts_dirty = true
    end
    log.info(string.format("MCP prompt registered: %s", name))
end

--- Remove an MCP prompt by name.
-- @param name string Prompt name
function M.remove_prompt(name)
    if prompts[name] then
        prompts[name] = nil
        if not _batch then
            schedule_notify_prompts()
        else
            _batch_prompts_dirty = true
        end
        log.info(string.format("MCP prompt removed: %s", name))
    end
end

--- List all registered prompts (metadata only, no handlers).
-- @return array of { name, description, [arguments] }
-- Note: arguments is omitted when empty. An empty Lua table serializes as {}
-- (JSON object) rather than [] (JSON array), which fails MCP schema validation.
-- The MCP spec marks arguments as optional, so omitting it is correct.
function M.list_prompts()
    local result = {}
    for _, prompt in pairs(prompts) do
        local entry = {
            name = prompt.name,
            description = prompt.description,
        }
        if prompt.arguments and #prompt.arguments > 0 then
            entry.arguments = prompt.arguments
        end
        result[#result + 1] = entry
    end
    return result
end

--- Get a prompt by name, executing its handler with the given arguments.
-- Returns a table conforming to the MCP prompts/get response shape:
--   { description = "...", messages = [ { role, content } ] }
-- Handlers may return this shape directly, or a plain string which is
-- wrapped into a single user message automatically.
-- @param name string Prompt name
-- @param args table Argument values from the MCP client (key = arg name)
-- @return result table|nil, error string|nil
function M.get_prompt(name, args)
    local prompt = prompts[name]
    if not prompt then
        return nil, "Unknown prompt: " .. name
    end

    local ok, result = pcall(prompt.handler, args or {})
    if not ok then
        return nil, tostring(result)
    end

    -- Normalize: plain string -> single user message
    if type(result) == "string" then
        return {
            description = prompt.description,
            messages = {
                { role = "user", content = { type = "text", text = result } },
            },
        }
    elseif type(result) == "table" then
        return result
    else
        return nil, "mcp.get_prompt: handler returned unexpected type: " .. type(result)
    end
end

--- Get count of registered prompts.
-- @return number
function M.count_prompts()
    local n = 0
    for _ in pairs(prompts) do n = n + 1 end
    return n
end

-- =============================================================================
-- Batch Updates (shared across tools and prompts)
-- =============================================================================

--- Clear tools and prompts registered by a specific source.
-- Called automatically before plugin reload. If source is nil, clears all.
-- @param source string|nil Source identifier to clear (nil = clear all)
function M.reset(source)
    local removed_tools = 0
    local removed_prompts = 0

    if source then
        for name, tool in pairs(tools) do
            if tool.source == source then
                tools[name] = nil
                removed_tools = removed_tools + 1
            end
        end
        for name, prompt in pairs(prompts) do
            if prompt.source == source then
                prompts[name] = nil
                removed_prompts = removed_prompts + 1
            end
        end
    else
        for name in pairs(tools) do
            tools[name] = nil
            removed_tools = removed_tools + 1
        end
        for name in pairs(prompts) do
            prompts[name] = nil
            removed_prompts = removed_prompts + 1
        end
    end

    if not _batch then
        if removed_tools > 0 then
            schedule_notify()
        end
        if removed_prompts > 0 then
            schedule_notify_prompts()
        end
    else
        if removed_tools > 0 then
            _batch_tools_dirty = true
        end
        if removed_prompts > 0 then
            _batch_prompts_dirty = true
        end
    end

    log.info(string.format(
        "MCP reset: %d tools, %d prompts removed (source=%s)",
        removed_tools, removed_prompts, tostring(source)
    ))
end

--- Begin a batch update — suppress notifications during reset + registration.
-- Always pair with end_batch(). Use pcall around load_plugin so end_batch()
-- runs even on failure — leaving batch mode stuck would permanently silence
-- notifications.
function M.begin_batch()
    _batch = true
end

--- End a batch update — emit changed notifications (debounced) for only the
-- registries that were actually modified during the batch. A tool-only plugin
-- reload will not fire mcp_prompts_changed; a prompt-only change will not
-- fire mcp_tools_changed. Correct even on load failure: if reset() cleared
-- registrations, the dirty flag was set and clients will be notified.
function M.end_batch()
    _batch = false
    local tools_dirty = _batch_tools_dirty
    local prompts_dirty = _batch_prompts_dirty
    _batch_tools_dirty = false
    _batch_prompts_dirty = false
    if tools_dirty then
        schedule_notify()
    end
    if prompts_dirty then
        schedule_notify_prompts()
    end
end

-- =============================================================================
-- Lifecycle Hooks for Hot-Reload
-- =============================================================================
-- mcp.lua holds plugin-registered tool and prompt handlers. On reload:
--   _before_reload: save both registries (handlers are plugin closures)
--   _after_reload:  restore both, update _G.mcp so existing call sites see new module
-- Without these hooks, reloading mcp.lua silently orphans all registrations.

function M._before_reload()
    -- Cancel any pending debounce timers — the reload cycle will schedule fresh ones.
    if _debounce_timer then
        timer.cancel(_debounce_timer)
        _debounce_timer = nil
    end
    if _debounce_timer_prompts then
        timer.cancel(_debounce_timer_prompts)
        _debounce_timer_prompts = nil
    end
    -- Stash both registries via hub.state (in-memory, handles survive reload)
    if _G.state then
        _G.state.set("mcp_tools_saved", tools)
        _G.state.set("mcp_prompts_saved", prompts)
    end
    log.info(string.format(
        "mcp.lua reloading — saving %d tools, %d prompts",
        M.count(), M.count_prompts()
    ))
end

function M._after_reload()
    -- Restore both registries from before reload
    if _G.state then
        local saved_tools = _G.state.get("mcp_tools_saved", {})
        local saved_prompts = _G.state.get("mcp_prompts_saved", {})
        tools = saved_tools
        prompts = saved_prompts
        _G.state.set("mcp_tools_saved", nil)
        _G.state.set("mcp_prompts_saved", nil)
    end
    -- Update global so callers using _G.mcp get new module methods
    _G.mcp = M
    log.info(string.format(
        "mcp.lua reloaded — %d tools, %d prompts preserved",
        M.count(), M.count_prompts()
    ))
end

return M
