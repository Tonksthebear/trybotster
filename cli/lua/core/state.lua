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

--- Get a persistent class metatable.
-- Returns the same table identity across hot-reloads, with old methods
-- cleared so renamed/deleted methods don't linger. Existing instances
-- using this table as their __index automatically see new methods.
--
-- Usage:
--   local MyClass = state.class("my_module.class")
--   function MyClass.new(...) ... end
--   function MyClass:some_method() ... end
--
-- @param key Unique key for this class (e.g., "client.class")
-- @return The persistent class table with __index set to itself
function M.class(key)
    local cls = M.get(key, {})
    for k, v in pairs(cls) do
        if type(v) == "function" then
            cls[k] = nil
        end
    end
    cls.__index = cls
    return cls
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
