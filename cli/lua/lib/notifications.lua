-- Scoped PTY notification policy registry.
--
-- Plugins may observe notifications without changing delivery, or claim
-- ownership for matching sessions and return a declarative delivery decision.
-- The hub remains responsible for badge/push/transient side effects.

local M = {}

local observers = {}
local claims = {}

local DEFAULT_TIMEOUT_MS = 5000

local function current_plugin_key()
    local key = rawget(_G, "_loading_plugin_key") or rawget(_G, "_loading_plugin_name")
    if type(key) == "string" and key ~= "" then return key end
    return nil
end

local function current_plugin_name()
    local name = rawget(_G, "_loading_plugin_display_name") or rawget(_G, "_loading_plugin_name")
    if type(name) == "string" and name ~= "" then return name end
    return nil
end

local function normalize_scope(scope)
    scope = scope or {}
    if type(scope) ~= "table" then
        error("notifications scope must be a table", 3)
    end
    return scope
end

local function has_capability(opts, capability)
    if opts[capability] == true then return true end
    if opts.capability == capability then return true end
    if type(opts.capabilities) == "table" then
        for _, value in ipairs(opts.capabilities) do
            if value == capability then return true end
        end
        if opts.capabilities[capability] == true then return true end
    end
    return false
end

local function assert_global_scope_allowed(kind, opts, scope)
    if scope.all_sessions ~= true then return end

    local capability = kind == "claim"
        and "notifications.global_claim"
        or "notifications.global_observe"
    if has_capability(opts, capability) then return end

    error(string.format(
        "notifications.%s: scope.all_sessions requires capability %q",
        kind, capability), 3)
end

local function contains(list, value)
    if type(list) ~= "table" then return false end
    for _, item in ipairs(list) do
        if item == value then return true end
    end
    return false
end

local function scope_matches(scope, intent)
    if type(intent) ~= "table" then return false end
    if scope.all_sessions == true then return true end

    local session_uuid = intent.session_uuid
    if type(scope.session_uuid) == "string" and scope.session_uuid == session_uuid then
        return true
    end
    if contains(scope.sessions, session_uuid) then
        return true
    end

    local owner = intent.session_owner_plugin or intent.owner_plugin
    if type(scope.owner_plugin) == "string" and scope.owner_plugin ~= "" and scope.owner_plugin == owner then
        return true
    end

    local surface = intent.session_surface or intent.surface
    if type(scope.surface) == "string" and scope.surface ~= "" and scope.surface == surface then
        return true
    end

    return false
end

local function sorted_entries(registry)
    local out = {}
    for name, entry in pairs(registry) do
        out[#out + 1] = { name = name, entry = entry }
    end
    table.sort(out, function(a, b)
        if a.entry.priority == b.entry.priority then
            return a.name < b.name
        end
        return a.entry.priority > b.entry.priority
    end)
    return out
end

local function invoke(entry, label, handler_kind, payload, ...)
    return require("lib.plugin_supervisor").invoke(
        entry.owner_plugin,
        label,
        entry.handler,
        {
            timeout_ms = entry.timeout_ms or DEFAULT_TIMEOUT_MS,
            handler_kind = handler_kind,
            handler_id = entry.name,
            payload = payload,
        },
        ...)
end

local function normalize_decision(value)
    if type(value) ~= "table" then
        return { core = "default" }
    end

    local core = value.core or value.delivery or "default"
    if core ~= "default" and core ~= "suppress" and core ~= "replace" then
        core = "default"
    end
    value.core = core
    return value
end

--- Register a notification observer.
-- Observers can watch matching notification intents but cannot affect delivery.
-- @param opts table { name, scope, handler, priority?, timeout_ms?, phase? }
function M.observe(opts)
    assert(type(opts) == "table", "notifications.observe: opts table required")
    local name = opts.name or opts.id
    assert(type(name) == "string" and name ~= "", "notifications.observe: name required")
    assert(type(opts.handler) == "function", "notifications.observe: handler function required")

    local scope = normalize_scope(opts.scope)
    assert_global_scope_allowed("observe", opts, scope)

    observers[name] = {
        name = name,
        scope = scope,
        handler = opts.handler,
        priority = opts.priority or 100,
        timeout_ms = opts.timeout_ms or DEFAULT_TIMEOUT_MS,
        phase = opts.phase or "both",
        owner_plugin = opts.owner_plugin or current_plugin_key(),
        plugin = opts.plugin or current_plugin_name(),
    }
end

--- Register a notification ownership claim.
-- The first matching claim by priority decides delivery for that notification.
-- @param opts table { name, scope, handler, priority?, timeout_ms? }
function M.claim(opts)
    assert(type(opts) == "table", "notifications.claim: opts table required")
    local name = opts.name or opts.id
    assert(type(name) == "string" and name ~= "", "notifications.claim: name required")
    assert(type(opts.handler) == "function", "notifications.claim: handler function required")

    local scope = normalize_scope(opts.scope)
    assert_global_scope_allowed("claim", opts, scope)

    claims[name] = {
        name = name,
        scope = scope,
        handler = opts.handler,
        priority = opts.priority or 100,
        timeout_ms = opts.timeout_ms or DEFAULT_TIMEOUT_MS,
        owner_plugin = opts.owner_plugin or current_plugin_key(),
        plugin = opts.plugin or current_plugin_name(),
    }
end

function M.unobserve(name)
    observers[name] = nil
end

function M.unclaim(name)
    claims[name] = nil
end

function M.unregister_by_plugin(plugin_key)
    if type(plugin_key) ~= "string" or plugin_key == "" then return 0 end
    local removed = 0
    for name, entry in pairs(observers) do
        if entry.owner_plugin == plugin_key then
            observers[name] = nil
            removed = removed + 1
        end
    end
    for name, entry in pairs(claims) do
        if entry.owner_plugin == plugin_key then
            claims[name] = nil
            removed = removed + 1
        end
    end
    return removed
end

function M.notify_observers(phase, intent, decision)
    local count = 0
    for _, wrapped in ipairs(sorted_entries(observers)) do
        local entry = wrapped.entry
        if (entry.phase == "both" or entry.phase == phase) and scope_matches(entry.scope, intent) then
            local ok, err = invoke(
                entry,
                "notification_observer:" .. entry.name,
                "notification_observer",
                { phase = phase, intent = intent, decision = decision },
                phase,
                intent,
                decision)
            if not ok then
                log.warn(string.format(
                    "notification observer %s failed: %s",
                    entry.name, tostring(err)))
            end
            count = count + 1
        end
    end
    return count
end

function M.evaluate(intent)
    M.notify_observers("before", intent, nil)

    for _, wrapped in ipairs(sorted_entries(claims)) do
        local entry = wrapped.entry
        if scope_matches(entry.scope, intent) then
            local ok, result = invoke(
                entry,
                "notification_claim:" .. entry.name,
                "notification_claim",
                { intent = intent },
                intent)
            if ok then
                local decision = normalize_decision(result)
                decision.owner = entry.name
                decision.owner_plugin = entry.owner_plugin
                return decision
            end

            log.warn(string.format(
                "notification claim %s failed, falling back to default behavior: %s",
                entry.name, tostring(result)))
            return {
                core = "default",
                owner = entry.name,
                owner_plugin = entry.owner_plugin,
                error = tostring(result),
            }
        end
    end

    return { core = "default" }
end

function M._invoke_observer(name, phase, intent, decision)
    local entry = observers[name]
    if not entry then error("notification observer not registered: " .. tostring(name)) end
    return entry.handler(phase, intent, decision)
end

function M._invoke_claim(name, intent)
    local entry = claims[name]
    if not entry then error("notification claim not registered: " .. tostring(name)) end
    return entry.handler(intent)
end

function M._reset_for_tests()
    for name in pairs(observers) do observers[name] = nil end
    for name in pairs(claims) do claims[name] = nil end
end

return M
