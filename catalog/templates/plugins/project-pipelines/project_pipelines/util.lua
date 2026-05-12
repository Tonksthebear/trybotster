-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/util.lua
-- @scope device
-- @version 1.1.0

local M = {}

math.randomseed(os.time())

function M.now()
    return os.time()
end

function M.id(prefix)
    local n = math.random(100000, 999999)
    return string.format("%s_%d_%d", prefix, os.time(), n)
end

function M.encode(value)
    if value == nil then
        return "{}"
    end
    local ok, encoded = pcall(json.encode, value)
    if ok and encoded then
        return encoded
    end
    return "{}"
end

function M.decode(value, fallback)
    if value == nil or value == "" then
        return fallback
    end
    local ok, decoded = pcall(json.decode, value)
    if ok and decoded ~= nil then
        return decoded
    end
    return fallback
end

function M.copy(row)
    local out = {}
    for key, value in pairs(row or {}) do
        out[key] = value
    end
    return out
end

function M.first(rows)
    if rows and #rows > 0 then
        return rows[1]
    end
    return nil
end

function M.is_blank(value)
    return value == nil or tostring(value):match("^%s*$") ~= nil
end

function M.assert_present(value, name)
    if M.is_blank(value) then
        error(name .. " is required")
    end
    return value
end

function M.safe_text(value)
    if value == nil then
        return ""
    end
    return tostring(value)
end

function M.list_contains(values, needle)
    for _, value in ipairs(values or {}) do
        if value == needle then
            return true
        end
    end
    return false
end

return M
