-- Shared stable URL facade.
--
-- The hub keeps descriptors for discovery/routing. Executable behavior belongs
-- to the owning plugin worker and is invoked through stable handler ids.

local state = require("hub.state")

local registry = state.get("stable_urls.registry", {})

local M = {}

local function current_plugin_key()
    local key = rawget(_G, "_loading_plugin_key") or rawget(_G, "_loading_plugin_name")
    if type(key) == "string" and key ~= "" then return key end
    return nil
end

local function in_plugin_worker()
    return type(rawget(_G, "_plugin_worker_key")) == "string"
end

local function parent_call(operation, params)
    local bridge = rawget(_G, "plugin_worker_parent_hub")
    if not bridge or type(bridge.request) ~= "function" then
        error("stable_urls." .. operation .. ": parent hub bridge is unavailable")
    end
    local response = bridge.request({
        type = "stable_urls_call",
        operation = operation,
        params = params or {},
    }, 10000)
    if response and response.error then
        error(response.error)
    end
    return response and response.result or nil
end

function M.register(operation, handler, opts)
    assert(type(operation) == "string" and operation ~= "", "stable_urls.register: operation required")
    assert(type(handler) == "function", "stable_urls.register: handler must be a function")
    opts = opts or {}
    local owner_plugin = opts.owner_plugin or current_plugin_key()
    assert(type(owner_plugin) == "string" and owner_plugin ~= "", "stable_urls.register: owner_plugin required")
    local handler_id = opts.handler_id or ("stable_urls:" .. operation)

    registry[operation] = {
        operation = operation,
        handler = handler,
        owner_plugin = owner_plugin,
        handler_id = handler_id,
        timeout_ms = opts.timeout_ms or 5000,
    }
end

function M.unregister_by_plugin(plugin_key)
    if type(plugin_key) ~= "string" or plugin_key == "" then return 0 end
    local removed = 0
    for operation, entry in pairs(registry) do
        if entry.owner_plugin == plugin_key then
            registry[operation] = nil
            removed = removed + 1
        end
    end
    return removed
end

function M._invoke_registered(handler_id, params, context)
    for _, entry in pairs(registry) do
        if entry.handler_id == handler_id then
            return entry.handler(params or {}, context or {})
        end
    end
    error("stable_urls handler not registered: " .. tostring(handler_id))
end

function M.call(operation, params, context)
    if in_plugin_worker() then
        return parent_call(operation, params)
    end

    local entry = registry[operation]
    if not entry then
        error("stable_urls." .. tostring(operation) .. ": no provider registered")
    end

    local ok, result = require("lib.plugin_supervisor").invoke(
        entry.owner_plugin,
        "stable_urls:" .. operation,
        entry.handler,
        {
            timeout_ms = entry.timeout_ms or 5000,
            handler_kind = "stable_urls_api",
            handler_id = entry.handler_id,
            payload = {
                operation = operation,
                params = params or {},
                context = context or {},
            },
        },
        params or {},
        context or {})
    if not ok then
        error(result)
    end
    return result
end

function M.claim(params) return M.call("claim", params or {}) end
function M.release(params) return M.call("release", params or {}) end
function M.list(params) return M.call("list", params or {}) end
function M.get(params) return M.call("get", params or {}) end

return M
