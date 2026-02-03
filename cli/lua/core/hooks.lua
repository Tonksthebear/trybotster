-- Hook system for event callbacks
local M = {}

-- Storage: hooks[event_name][hook_name] = { callback, priority, enabled }
local hooks = {}

-- Register a hook
-- options: { priority = 100, enabled = true }
function M.register(event_name, hook_name, callback, options)
    options = options or {}
    local priority = options.priority or 100
    local enabled = options.enabled ~= false

    if not hooks[event_name] then
        hooks[event_name] = {}
    end

    hooks[event_name][hook_name] = {
        callback = callback,
        priority = priority,
        enabled = enabled,
    }

    log.debug(string.format("Hook registered: %s.%s (priority=%d)", event_name, hook_name, priority))
end

-- Unregister a hook
function M.unregister(event_name, hook_name)
    if hooks[event_name] then
        hooks[event_name][hook_name] = nil
        log.debug(string.format("Hook unregistered: %s.%s", event_name, hook_name))
    end
end

-- Enable/disable without removing
function M.enable(event_name, hook_name)
    if hooks[event_name] and hooks[event_name][hook_name] then
        hooks[event_name][hook_name].enabled = true
    end
end

function M.disable(event_name, hook_name)
    if hooks[event_name] and hooks[event_name][hook_name] then
        hooks[event_name][hook_name].enabled = false
    end
end

-- Check if any hooks are registered for an event
function M.has(event_name)
    if not hooks[event_name] then
        return false
    end
    for _, hook in pairs(hooks[event_name]) do
        if hook.enabled then
            return true
        end
    end
    return false
end

--- Execute hook chain (sorted by priority, higher first).
--
-- LIMITATIONS:
-- - Multi-value returns are NOT preserved through the chain. Only the first
--   return value from each hook is passed to the next hook.
-- - If a hook returns nil (explicit or implicit), the data is DROPPED and
--   nil is returned to the caller. Hooks that want to pass data through
--   MUST return a value.
-- - When no hooks are registered, returns the original arguments unchanged.
--
-- @param event_name The event to fire
-- @param ... Arguments to pass to hooks
-- @return Transformed data from hook chain, or nil if any hook dropped it
function M.call(event_name, ...)
    if not hooks[event_name] then
        return ...
    end

    -- Sort hooks by priority (higher priority first)
    local sorted = {}
    for name, hook in pairs(hooks[event_name]) do
        if hook.enabled then
            table.insert(sorted, { name = name, hook = hook })
        end
    end
    table.sort(sorted, function(a, b) return a.hook.priority > b.hook.priority end)

    -- Execute chain
    local result = { ... }
    for _, entry in ipairs(sorted) do
        local ok, new_result = pcall(entry.hook.callback, table.unpack(result))
        if not ok then
            log.error(string.format("Hook %s.%s error: %s", event_name, entry.name, tostring(new_result)))
            -- Continue with previous result on error
        elseif new_result == nil then
            -- Hook returned nil (explicit or implicit), signal to drop data.
            -- WARNING: Hooks that forget to return a value will drop data!
            return nil
        else
            result = { new_result }
        end
    end

    return table.unpack(result)
end

-- List hooks for an event (for debugging)
function M.list(event_name)
    local result = {}
    if hooks[event_name] then
        for name, hook in pairs(hooks[event_name]) do
            table.insert(result, {
                name = name,
                priority = hook.priority,
                enabled = hook.enabled,
            })
        end
    end
    return result
end

return M
