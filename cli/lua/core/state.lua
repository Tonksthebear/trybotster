-- State persistence across hot-reloads
-- This module is loaded once and never reloaded

local M = {}
local store = {}

-- Get or initialize state for a key
-- Returns the same table on every call (survives reload)
function M.get(key, default)
    if store[key] == nil then
        store[key] = default or {}
    end
    return store[key]
end

-- Set state for a key
function M.set(key, value)
    store[key] = value
end

-- Clear state for a key
function M.clear(key)
    store[key] = nil
end

-- List all keys (for debugging)
function M.keys()
    local result = {}
    for k in pairs(store) do
        table.insert(result, k)
    end
    return result
end

return M
