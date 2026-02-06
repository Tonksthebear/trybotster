-- Hook system: observers and interceptors
--
-- OBSERVERS: Async, safe, fire-and-forget. Cannot block or transform data.
--   hooks.on("pty_output", "my_logger", fn)
--   hooks.notify("pty_output", ctx, data)
--
-- INTERCEPTORS: Sync, blocking, can transform/drop. Use sparingly.
--   hooks.intercept("pty_output", "my_filter", fn, { timeout_ms = 10 })
--   hooks.call("pty_output", ctx, data) -> transformed or nil
--
local M = {}

-- Storage: observers[event][name] = { callback, priority, enabled }
local observers = {}

-- Storage: interceptors[event][name] = { callback, priority, enabled, timeout_ms }
local interceptors = {}

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
    observers[event][name] = {
        callback = callback,
        priority = opts.priority or 100,
        enabled = opts.enabled ~= false,
    }
    log.debug(string.format("hooks.on: %s.%s", event, name))
end

--- Remove an observer.
function M.off(event, name)
    if observers[event] then
        observers[event][name] = nil
    end
end

--- Check if observers exist for an event.
function M.has_observers(event)
    if not observers[event] then return false end
    for _, h in pairs(observers[event]) do
        if h.enabled then return true end
    end
    return false
end

--- Notify all observers (fire-and-forget). Errors logged, not propagated.
--- @param event string Event name
--- @param ... any Arguments passed to observers
--- @return number Number of observers called
function M.notify(event, ...)
    if not observers[event] then return 0 end

    local sorted = {}
    for name, h in pairs(observers[event]) do
        if h.enabled then
            table.insert(sorted, { name = name, h = h })
        end
    end
    table.sort(sorted, function(a, b) return a.h.priority > b.h.priority end)

    local count = 0
    local args = { ... }
    for _, entry in ipairs(sorted) do
        local ok, err = pcall(entry.h.callback, table.unpack(args))
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
    interceptors[event][name] = {
        callback = callback,
        priority = opts.priority or 100,
        enabled = opts.enabled ~= false,
        timeout_ms = opts.timeout_ms or 10,
    }
    log.debug(string.format("hooks.intercept: %s.%s (timeout=%dms)",
        event, name, opts.timeout_ms or 10))
end

--- Remove an interceptor.
function M.unintercept(event, name)
    if interceptors[event] then
        interceptors[event][name] = nil
    end
end

--- Check if interceptors exist for an event.
function M.has_interceptors(event)
    if not interceptors[event] then return false end
    for _, h in pairs(interceptors[event]) do
        if h.enabled then return true end
    end
    return false
end

--- Call interceptor chain. Each can transform or drop (return nil).
--- @param event string Event name
--- @param ... any Arguments passed through chain
--- @return any Transformed result, or nil if dropped
function M.call(event, ...)
    if not interceptors[event] then return ... end

    local sorted = {}
    for name, h in pairs(interceptors[event]) do
        if h.enabled then
            table.insert(sorted, { name = name, h = h })
        end
    end
    table.sort(sorted, function(a, b) return a.h.priority > b.h.priority end)

    local result = { ... }
    for _, entry in ipairs(sorted) do
        local ok, new_result = pcall(entry.h.callback, table.unpack(result))
        if not ok then
            log.error(string.format("hooks.call %s.%s: %s", event, entry.name, new_result))
            -- Continue with previous result on error
        elseif new_result == nil then
            return nil -- Dropped
        else
            result = { new_result }
        end
    end
    return table.unpack(result)
end

-- =============================================================================
-- UTILITIES
-- =============================================================================

--- Enable a hook (observer or interceptor).
function M.enable(event, name)
    if observers[event] and observers[event][name] then
        observers[event][name].enabled = true
    end
    if interceptors[event] and interceptors[event][name] then
        interceptors[event][name].enabled = true
    end
end

--- Disable a hook without removing it.
function M.disable(event, name)
    if observers[event] and observers[event][name] then
        observers[event][name].enabled = false
    end
    if interceptors[event] and interceptors[event][name] then
        interceptors[event][name].enabled = false
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
