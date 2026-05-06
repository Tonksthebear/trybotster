-- Hook system: observers and interceptors
--
-- OBSERVERS: Async, safe, fire-and-forget. Cannot block or transform data.
--   hooks.on("agent_created", "my_logger", fn)
--   hooks.notify("agent_created", data)
--
-- INTERCEPTORS: Sync, blocking, can transform/drop control-plane data. Use sparingly.
--   hooks.intercept("before_client_subscribe", "my_filter", fn, { timeout_ms = 10 })
--   hooks.call("before_client_subscribe", data) -> transformed or nil
--
local M = {}

-- Storage: observers[event][name] = { callback, priority, enabled }
local observers = {}

-- Storage: interceptors[event][name] = { callback, priority, enabled, timeout_ms }
local interceptors = {}

-- Sorted list caches: event -> sorted array (nil = dirty)
local observer_cache = {}
local interceptor_cache = {}

-- Enabled counts: event -> number of enabled hooks (for fast has_* checks)
local observer_enabled_count = {}
local interceptor_enabled_count = {}

local function invalidate_observer_cache(event)
    observer_cache[event] = nil
end

local function invalidate_interceptor_cache(event)
    interceptor_cache[event] = nil
end

local function current_plugin_key()
    local key = rawget(_G, "_loading_plugin_key") or rawget(_G, "_loading_plugin_name")
    if type(key) == "string" and key ~= "" then return key end
    return nil
end

local function get_sorted_observers(event)
    if observer_cache[event] then return observer_cache[event] end
    local sorted = {}
    if observers[event] then
        for name, h in pairs(observers[event]) do
            if h.enabled then
                table.insert(sorted, { name = name, h = h })
            end
        end
        table.sort(sorted, function(a, b) return a.h.priority > b.h.priority end)
    end
    observer_cache[event] = sorted
    return sorted
end

local function get_sorted_interceptors(event)
    if interceptor_cache[event] then return interceptor_cache[event] end
    local sorted = {}
    if interceptors[event] then
        for name, h in pairs(interceptors[event]) do
            if h.enabled then
                table.insert(sorted, { name = name, h = h })
            end
        end
        table.sort(sorted, function(a, b) return a.h.priority > b.h.priority end)
    end
    interceptor_cache[event] = sorted
    return sorted
end

-- =============================================================================
-- OBSERVERS
-- =============================================================================

--- Register an observer. Observers are notified asynchronously and cannot
--- affect data flow. Use for logging, metrics, side effects.
---
--- @param event string Event name
--- @param name string Unique hook name
--- @param callback function Called with event args (return value ignored)
--- @param opts? table { priority = 100, enabled = true }
function M.on(event, name, callback, opts)
    opts = opts or {}
    observers[event] = observers[event] or {}
    local enabled = opts.enabled ~= false

    -- Track enabled count: if replacing an existing hook, adjust accordingly
    local old = observers[event][name]
    if old then
        if old.enabled and not enabled then
            observer_enabled_count[event] = (observer_enabled_count[event] or 1) - 1
        elseif not old.enabled and enabled then
            observer_enabled_count[event] = (observer_enabled_count[event] or 0) + 1
        end
    elseif enabled then
        observer_enabled_count[event] = (observer_enabled_count[event] or 0) + 1
    end

    observers[event][name] = {
        callback = callback,
        priority = opts.priority or 100,
        enabled = enabled,
        owner_plugin = opts.owner_plugin or current_plugin_key(),
        timeout_ms = opts.timeout_ms or 5000,
    }
    invalidate_observer_cache(event)
    log.debug(string.format("hooks.on: %s.%s", event, name))
end

--- Remove an observer.
function M.off(event, name)
    if observers[event] and observers[event][name] then
        if observers[event][name].enabled then
            observer_enabled_count[event] = (observer_enabled_count[event] or 1) - 1
        end
        observers[event][name] = nil
        invalidate_observer_cache(event)
    end
end

function M.unregister_by_plugin(plugin_key)
    if type(plugin_key) ~= "string" or plugin_key == "" then return 0 end
    local prefix = plugin_key .. "::"
    local removed = 0

    for event, entries in pairs(observers) do
        for name, hook in pairs(entries) do
            if hook.owner_plugin == plugin_key or (type(name) == "string" and name:sub(1, #prefix) == prefix) then
                if hook.enabled then
                    observer_enabled_count[event] = (observer_enabled_count[event] or 1) - 1
                end
                entries[name] = nil
                removed = removed + 1
                invalidate_observer_cache(event)
            end
        end
    end

    for event, entries in pairs(interceptors) do
        for name, hook in pairs(entries) do
            if hook.owner_plugin == plugin_key or (type(name) == "string" and name:sub(1, #prefix) == prefix) then
                if hook.enabled then
                    interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 1) - 1
                end
                entries[name] = nil
                removed = removed + 1
                invalidate_interceptor_cache(event)
            end
        end
    end

    return removed
end

--- Check if observers exist for an event.
function M.has_observers(event)
    return (observer_enabled_count[event] or 0) > 0
end

--- Notify all observers (fire-and-forget). Errors logged, not propagated.
--- @param event string Event name
--- @param ... any Arguments passed to observers
--- @return number Number of observers called
function M.notify(event, ...)
    if (observer_enabled_count[event] or 0) == 0 then return 0 end

    local sorted = get_sorted_observers(event)
    local count = 0
    local args = { ... }
    for _, entry in ipairs(sorted) do
        local ok, err
        if entry.h.owner_plugin then
            ok, err = require("lib.plugin_supervisor").invoke(
                entry.h.owner_plugin,
                "hook_observer:" .. tostring(event) .. ":" .. tostring(entry.name),
                entry.h.callback,
                {
                    timeout_ms = entry.h.timeout_ms or 5000,
                    handler_kind = "hook_observer",
                    handler_id = event,
                    handler_name = entry.name,
                    payload = { args = args },
                },
                table.unpack(args))
        else
            ok, err = pcall(entry.h.callback, table.unpack(args))
        end
        if not ok then
            log.error(string.format("hooks.notify %s.%s: %s", event, entry.name, err))
        end
        count = count + 1
    end
    return count
end

-- =============================================================================
-- INTERCEPTORS
-- =============================================================================

--- Register an interceptor. Interceptors run synchronously and can transform
--- or drop data. WARNING: Blocks the pipeline. Use timeout_ms to limit damage.
---
--- @param event string Event name
--- @param name string Unique hook name
--- @param callback function Must return transformed data or nil to drop
--- @param opts? table { priority = 100, enabled = true, timeout_ms = 10 }
function M.intercept(event, name, callback, opts)
    opts = opts or {}
    interceptors[event] = interceptors[event] or {}
    local enabled = opts.enabled ~= false

    -- Track enabled count: if replacing an existing hook, adjust accordingly
    local old = interceptors[event][name]
    if old then
        if old.enabled and not enabled then
            interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 1) - 1
        elseif not old.enabled and enabled then
            interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 0) + 1
        end
    elseif enabled then
        interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 0) + 1
    end

    interceptors[event][name] = {
        callback = callback,
        priority = opts.priority or 100,
        enabled = enabled,
        timeout_ms = opts.timeout_ms or 10,
        owner_plugin = opts.owner_plugin or current_plugin_key(),
    }
    invalidate_interceptor_cache(event)
    log.debug(string.format("hooks.intercept: %s.%s (timeout=%dms)",
        event, name, opts.timeout_ms or 10))
end

--- Remove an interceptor.
function M.unintercept(event, name)
    if interceptors[event] and interceptors[event][name] then
        if interceptors[event][name].enabled then
            interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 1) - 1
        end
        interceptors[event][name] = nil
        invalidate_interceptor_cache(event)
    end
end

--- Check if interceptors exist for an event.
function M.has_interceptors(event)
    return (interceptor_enabled_count[event] or 0) > 0
end

--- Call interceptor chain. Each can transform or drop (return nil).
---
--- Each interceptor runs under a wall-deadline enforced by the Rust-side
--- `__hook_timed_pcall` primitive (see `cli/src/lua/primitives/hook_timeout.rs`).
--- If an interceptor's body blows past `timeout_ms`, a Lua VM hook raises a
--- runtime error; the chain continues with the previous (untransformed) value.
--- This catches pure-Lua infinite loops but NOT callbacks blocked inside a
--- Rust primitive; the Rust shutdown watchdog is the backstop for those.
---
--- @param event string Event name
--- @param ... any Arguments passed through chain
--- @return any Transformed result, or nil if dropped
function M.call(event, ...)
    if (interceptor_enabled_count[event] or 0) == 0 then return ... end

    local sorted = get_sorted_interceptors(event)

    local result = table.pack(...)
    for _, entry in ipairs(sorted) do
        local timeout_ms = entry.h.timeout_ms or 10
        local returns
        if entry.h.owner_plugin then
            returns = table.pack(require("lib.plugin_supervisor").invoke(
                entry.h.owner_plugin,
                "hook_interceptor:" .. tostring(event) .. ":" .. tostring(entry.name),
                entry.h.callback,
                {
                    timeout_ms = timeout_ms,
                    handler_kind = "hook_interceptor",
                    handler_id = event,
                    handler_name = entry.name,
                    payload = { args = { table.unpack(result, 1, result.n) } },
                },
                table.unpack(result, 1, result.n)))
        else
            returns = table.pack(__hook_timed_pcall(
                entry.h.callback, timeout_ms, table.unpack(result, 1, result.n)))
        end

        local ok = returns[1]
        if not ok then
            log.error(string.format("hooks.call %s.%s: %s", event, entry.name, returns[2]))
            -- Continue with previous result on error (timeout or otherwise)
        elseif returns.n <= 1 or returns[2] == nil then
            return nil -- Dropped (no return values or explicit nil)
        else
            result = table.pack(select(2, table.unpack(returns, 1, returns.n)))
        end
    end
    return table.unpack(result, 1, result.n)
end

function M._invoke_observer(event, name, args)
    local h = observers[event] and observers[event][name]
    if not h then error("hook observer not registered: " .. tostring(event) .. ":" .. tostring(name)) end
    return h.callback(table.unpack(args or {}))
end

function M._invoke_interceptor(event, name, args)
    local h = interceptors[event] and interceptors[event][name]
    if not h then error("hook interceptor not registered: " .. tostring(event) .. ":" .. tostring(name)) end
    return h.callback(table.unpack(args or {}))
end

-- =============================================================================
-- UTILITIES
-- =============================================================================

--- Enable a hook (observer or interceptor).
function M.enable(event, name)
    if observers[event] and observers[event][name] and not observers[event][name].enabled then
        observers[event][name].enabled = true
        observer_enabled_count[event] = (observer_enabled_count[event] or 0) + 1
        invalidate_observer_cache(event)
    end
    if interceptors[event] and interceptors[event][name] and not interceptors[event][name].enabled then
        interceptors[event][name].enabled = true
        interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 0) + 1
        invalidate_interceptor_cache(event)
    end
end

--- Disable a hook without removing it.
function M.disable(event, name)
    if observers[event] and observers[event][name] and observers[event][name].enabled then
        observers[event][name].enabled = false
        observer_enabled_count[event] = (observer_enabled_count[event] or 1) - 1
        invalidate_observer_cache(event)
    end
    if interceptors[event] and interceptors[event][name] and interceptors[event][name].enabled then
        interceptors[event][name].enabled = false
        interceptor_enabled_count[event] = (interceptor_enabled_count[event] or 1) - 1
        invalidate_interceptor_cache(event)
    end
end

--- List all hooks for an event.
function M.list(event)
    local result = {}
    if observers[event] then
        for name, h in pairs(observers[event]) do
            table.insert(result, {
                name = name,
                type = "observer",
                priority = h.priority,
                enabled = h.enabled,
                owner_plugin = h.owner_plugin,
            })
        end
    end
    if interceptors[event] then
        for name, h in pairs(interceptors[event]) do
            table.insert(result, {
                name = name,
                type = "interceptor",
                priority = h.priority,
                enabled = h.enabled,
                timeout_ms = h.timeout_ms,
                owner_plugin = h.owner_plugin,
            })
        end
    end
    return result
end

--- List all events with registered hooks.
function M.list_events()
    local seen = {}
    local result = {}
    for event in pairs(observers) do
        if not seen[event] then
            table.insert(result, event)
            seen[event] = true
        end
    end
    for event in pairs(interceptors) do
        if not seen[event] then
            table.insert(result, event)
            seen[event] = true
        end
    end
    table.sort(result)
    return result
end

return M
