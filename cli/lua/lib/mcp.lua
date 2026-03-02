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

-- Internal tool registry: name -> { name, description, input_schema, handler, source, proxy_id? }
-- proxy_id is set for tools forwarded from a remote MCP server; handler is nil for these.
local tools = {}

-- Internal prompt registry: name -> { name, description, arguments, handler, source }
local prompts = {}

-- Proxy registry: proxy_id (url) -> { url, token, source, tool_names = {}, on_auth_error = fn|nil }
-- Tracks which remote MCP servers have been registered so we can clean up on reset/reload.
local proxies = {}

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

--- Build HTTP headers for a remote MCP server request.
-- @param token string|nil Bearer token for Authorization header
-- @param session_id string|nil MCP session ID (required after initialize handshake)
-- @return table HTTP headers
local function build_headers(token, session_id)
    local h = {
        ["Content-Type"] = "application/json",
        ["Accept"]       = "application/json",
    }
    if token then
        h["Authorization"] = "Bearer " .. token
    end
    if session_id then
        h["Mcp-Session-Id"] = session_id
    end
    return h
end

--- Parse an SSE (Server-Sent Events) response body and return the first data payload.
-- ActionMCP may respond with text/event-stream even when Accept: application/json is sent.
-- We extract the first non-empty "data: ..." line, which contains the JSON-RPC response.
-- @param body string Raw SSE response body
-- @return string|nil Extracted data payload, or nil if no data line found
local function parse_sse_body(body)
    for line in body:gmatch("[^\r\n]+") do
        local value = line:match("^data:%s*(.*)")
        if value and value ~= "" then
            return value
        end
    end
    return nil
end

--- Decode an HTTP response from a remote MCP server, handling both JSON and SSE formats.
-- Returns (data, err) where data is the decoded JSON-RPC envelope.
-- @param resp table HTTP response { status, body, headers }
-- @param url string Server URL (for error messages only)
-- @return table|nil, string|nil
local function decode_mcp_response(resp, url)
    local content_type = (resp.headers and resp.headers["content-type"]) or ""
    local raw
    if content_type:find("text/event-stream", 1, true) then
        raw = parse_sse_body(resp.body)
        if not raw then
            return nil, string.format("SSE response from %s had no data: line", url)
        end
    else
        raw = resp.body
    end
    local data, decode_err = json.decode(raw)
    if not data then
        return nil, string.format("JSON decode error from %s: %s", url, tostring(decode_err))
    end
    return data, nil
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

--- Normalize a tool result to MCP content array format.
-- @param result any Raw return value from a tool handler
-- @return table MCP content array: [{ type = "text", text = "..." }, ...]
local function normalize_result(result)
    if type(result) == "string" then
        return { { type = "text", text = result } }
    elseif type(result) == "table" then
        -- Already MCP content format (array of {type, text}) — pass through
        if result[1] and result[1].type then
            return result
        else
            return { { type = "text", text = json.encode(result) } }
        end
    else
        return { { type = "text", text = tostring(result) } }
    end
end

--- Call a tool by name.
--
-- Supports both synchronous (local tools) and asynchronous (proxied tools) dispatch.
-- When `callback` is provided, it is always called — synchronously for local tools,
-- asynchronously for proxied tools (after the HTTP response arrives).
-- Without `callback`, only local tools return immediately; proxied tools cannot be
-- called without a callback.
--
-- @param name string Tool name
-- @param params table Arguments from the MCP client
-- @param context table|nil Caller context (caller_cwd, etc.)
-- @param callback function|nil function(content, err) — if provided, result is delivered here
-- @return result, error_string (only meaningful for local tools without a callback)
function M.call_tool(name, params, context, callback)
    local tool = tools[name]
    if not tool then
        local err = "Unknown tool: " .. name
        if callback then callback(nil, err) return end
        return nil, err
    end

    -- Proxied tool: forward the call to the remote MCP server via HTTP.
    -- Always async; a callback is required.
    if tool.proxy_id then
        if not callback then
            log.warn(string.format(
                "mcp.call_tool: '%s' is a proxied tool — a callback is required, result will be lost",
                name
            ))
        end

        local proxy = proxies[tool.proxy_id]
        if not proxy then
            local err = "Proxy not found for tool: " .. name
            if callback then callback(nil, err) return end
            return nil, err
        end

        local body = json.encode({
            jsonrpc = "2.0",
            id      = 1,
            method  = "tools/call",
            params  = { name = name, arguments = params or {} },
        })

        http.request({
            method  = "POST",
            url     = proxy.url,
            headers = build_headers(proxy.token, proxy.session_id),
            body    = body,
        }, function(resp, http_err)
            if http_err then
                if callback then callback(nil, "HTTP error: " .. tostring(http_err)) end
                return
            end

            -- 401: token expired. Fire on_auth_error so the plugin can refresh, then report.
            if resp.status == 401 then
                if proxy.on_auth_error then proxy.on_auth_error() end
                if callback then
                    callback(nil, string.format("MCP token expired for %s (401)", proxy.url))
                end
                return
            end

            if resp.status ~= 200 then
                if callback then
                    callback(nil, string.format("Remote MCP error %d from %s", resp.status, proxy.url))
                end
                return
            end

            local data, decode_err = decode_mcp_response(resp, proxy.url)
            if not data then
                if callback then callback(nil, decode_err) end
                return
            end

            if data.error then
                local msg = (type(data.error) == "table" and data.error.message) or tostring(data.error)
                if callback then callback(nil, msg) end
                return
            end

            -- MCP tools/call result: { content: [...], isError: bool }
            local result = data.result or {}
            local content = result.content or {}
            local is_error = result.isError == true

            if is_error then
                local err_text = (content[1] and content[1].text) or "Remote tool error"
                if callback then callback(nil, err_text) end
            else
                if callback then callback(content, nil) end
            end
        end)

        return  -- async; result arrives via callback
    end

    -- Local tool: invoke handler synchronously.
    local ok, result = pcall(tool.handler, params or {}, context or {})
    if not ok then
        local err = tostring(result)
        if callback then callback(nil, err) return end
        return nil, err
    end

    local content = normalize_result(result)
    if callback then callback(content, nil) return end
    return content, nil
end

--- Get count of registered tools.
-- @return number
function M.count()
    local n = 0
    for _ in pairs(tools) do n = n + 1 end
    return n
end

-- =============================================================================
-- Remote MCP Proxy
-- =============================================================================

--- Register a remote MCP server as a proxy, merging its tools into the hub registry.
--
-- Fetches the remote server's tool list via MCP Streamable HTTP (POST, JSON-RPC
-- tools/list). Registered tools appear in mcp.list_tools() alongside local tools.
-- When called via mcp.call_tool(), proxied tools are forwarded to the remote server.
--
-- Safe to call repeatedly — acts as a refresh: removes old entries for this URL
-- and registers the freshly fetched set.
--
-- The source tag is set to the calling plugin's source so that mcp.reset() on
-- plugin unload automatically cleans up proxy registrations.
--
-- @param url string Remote MCP server URL (used as proxy_id key)
-- @param opts table|nil {
--   token = "bearer-token",        -- Authorization header for the remote server
--   on_auth_error = function()      -- Called when a tool call returns 401; use to refresh the token
-- }
function M.proxy(url, opts)
    if type(url) ~= "string" or url == "" then
        error("mcp.proxy: url must be a non-empty string")
    end
    opts = opts or {}
    local token        = opts.token
    local on_auth_error = opts.on_auth_error
    local source       = caller_source()
    local proxy_id     = url

    local init_body = json.encode({
        jsonrpc = "2.0",
        id      = 1,
        method  = "initialize",
        params  = {
            protocolVersion = "2024-11-05",
            capabilities    = {},
            clientInfo      = { name = "botster", version = "1.0" },
        },
    })

    -- Step 1: initialize to obtain Mcp-Session-Id, then fetch tools/list.
    http.request({
        method  = "POST",
        url     = url,
        headers = build_headers(token),
        body    = init_body,
    }, function(init_resp, init_err)
        if init_err then
            log.warn(string.format("mcp.proxy: initialize failed for %s: %s", url, tostring(init_err)))
            return
        end
        if init_resp.status ~= 200 then
            log.warn(string.format("mcp.proxy: initialize returned HTTP %d for %s", init_resp.status, url))
            return
        end

        local session_id = init_resp.headers and (
            init_resp.headers["mcp-session-id"] or init_resp.headers["Mcp-Session-Id"]
        )

        local list_body = json.encode({
            jsonrpc = "2.0",
            id      = 2,
            method  = "tools/list",
            params  = {},
        })

        http.request({
            method  = "POST",
            url     = url,
            headers = build_headers(token, session_id),
            body    = list_body,
        }, function(resp, http_err)
        if http_err then
            log.warn(string.format("mcp.proxy: failed to connect to %s: %s", url, tostring(http_err)))
            return
        end
        if resp.status ~= 200 then
            log.warn(string.format("mcp.proxy: %s returned HTTP %d", url, resp.status))
            return
        end

        local data, decode_err = decode_mcp_response(resp, url)
        if not data then
            log.warn(string.format("mcp.proxy: %s", decode_err))
            return
        end

        if data.error then
            local msg = (type(data.error) == "table" and data.error.message) or tostring(data.error)
            log.warn(string.format("mcp.proxy: remote error from %s: %s", url, msg))
            return
        end

        local remote_tools = (data.result and data.result.tools) or {}

        -- Preserve the original source (plugin file path) across timer-driven refreshes.
        -- caller_source() returns "unknown" outside a plugin-load context (e.g. timer
        -- callbacks), so a refresh would overwrite the source and cause mcp.reset(source)
        -- to miss these entries on plugin unload. Keep the existing source if one is set.
        local registered_source = (proxies[proxy_id] and proxies[proxy_id].source ~= "unknown")
            and proxies[proxy_id].source
            or source

        -- Batch all registrations into one notification cycle.
        M.begin_batch()

        -- Remove previous tools registered for this proxy_id (refresh semantics).
        if proxies[proxy_id] then
            for _, old_name in ipairs(proxies[proxy_id].tool_names or {}) do
                tools[old_name] = nil
                _batch_tools_dirty = true
            end
        end

        -- Register freshly fetched tools.
        local tool_names = {}
        for _, remote_tool in ipairs(remote_tools) do
            local tname = remote_tool.name
            if type(tname) == "string" and tname ~= "" then
                tools[tname] = {
                    name         = tname,
                    description  = remote_tool.description or "",
                    input_schema = remote_tool.inputSchema or { type = "object", properties = {} },
                    handler      = nil,               -- nil = proxied
                    source       = registered_source,
                    proxy_id     = proxy_id,
                }
                table.insert(tool_names, tname)
                _batch_tools_dirty = true
            end
        end

        -- Update proxy registry (preserve on_auth_error across refreshes if not re-supplied).
        local prev_on_auth_error = proxies[proxy_id] and proxies[proxy_id].on_auth_error
        proxies[proxy_id] = {
            url           = url,
            token         = token,
            session_id    = session_id,
            source        = registered_source,
            tool_names    = tool_names,
            on_auth_error = on_auth_error or prev_on_auth_error,
        }

        M.end_batch()

        log.info(string.format(
            "mcp.proxy: registered %d tools from %s",
            #tool_names, url
        ))
        end) -- tools/list callback
    end) -- initialize callback
end

--- Remove a proxy and all of its registered tools.
-- Fires debounced mcp_tools_changed if any tools were removed.
-- @param url string The proxy URL (same value passed to mcp.proxy)
function M.remove_proxy(url)
    local proxy = proxies[url]
    if not proxy then return end

    M.begin_batch()
    for _, tname in ipairs(proxy.tool_names or {}) do
        if tools[tname] and tools[tname].proxy_id == url then
            tools[tname] = nil
            _batch_tools_dirty = true
        end
    end
    proxies[url] = nil
    M.end_batch()

    log.info(string.format("mcp.proxy: removed proxy %s", url))
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

--- Clear tools, prompts, and proxies registered by a specific source.
-- Called automatically before plugin reload. If source is nil, clears all.
-- @param source string|nil Source identifier to clear (nil = clear all)
function M.reset(source)
    local removed_tools = 0
    local removed_prompts = 0
    local removed_proxies = 0

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
        -- Also remove any proxy entries registered by this source.
        for proxy_id, proxy in pairs(proxies) do
            if proxy.source == source then
                proxies[proxy_id] = nil
                removed_proxies = removed_proxies + 1
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
        for proxy_id in pairs(proxies) do
            proxies[proxy_id] = nil
            removed_proxies = removed_proxies + 1
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
        "MCP reset: %d tools, %d prompts, %d proxies removed (source=%s)",
        removed_tools, removed_prompts, removed_proxies, tostring(source)
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
    -- Stash all three registries via hub.state (in-memory, handles survive reload)
    if _G.state then
        _G.state.set("mcp_tools_saved", tools)
        _G.state.set("mcp_prompts_saved", prompts)
        _G.state.set("mcp_proxies_saved", proxies)
    end
    log.info(string.format(
        "mcp.lua reloading — saving %d tools, %d prompts, %d proxies",
        M.count(), M.count_prompts(), (function()
            local n = 0; for _ in pairs(proxies) do n = n + 1 end; return n
        end)()
    ))
end

function M._after_reload()
    -- Restore all three registries from before reload
    if _G.state then
        local saved_tools   = _G.state.get("mcp_tools_saved", {})
        local saved_prompts = _G.state.get("mcp_prompts_saved", {})
        local saved_proxies = _G.state.get("mcp_proxies_saved", {})
        tools   = saved_tools
        prompts = saved_prompts
        proxies = saved_proxies
        _G.state.set("mcp_tools_saved", nil)
        _G.state.set("mcp_prompts_saved", nil)
        _G.state.set("mcp_proxies_saved", nil)
    end
    -- Update global so callers using _G.mcp get new module methods
    _G.mcp = M
    log.info(string.format(
        "mcp.lua reloaded — %d tools, %d prompts preserved",
        M.count(), M.count_prompts()
    ))
end

return M
